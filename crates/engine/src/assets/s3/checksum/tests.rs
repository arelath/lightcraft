use super::*;
use crate::assets::s3::{AccessKeys, fixture::Fixture};
use crate::cloud_metadata::Cache;
use crate::cloud_replication::{SharedMedia, VerifiedMedia};
use lightcraft_catalog::assets::RemoteLocation;
use std::sync::Arc;
const DATA: &[u8] = b"abc";
const PATH: &str = "/photos/library/original";
fn fixture(bytes: &[u8]) -> (Fixture, S3Connection, Blob) {
    let f = Fixture::new();
    {
        let mut state = f.state.lock().unwrap();
        state.allow_typed_keys = true;
        state.aws_checksums = true;
        state.media_versions.insert(PATH.into(), "version-one".into());
    }
    let c: S3Connection =
        serde_json::from_value(serde_json::json!({"id":"personal","bucket":"photos","region":"us-east-1","prefix":"library"})).unwrap();
    let c = c
        .with_keys(AccessKeys::new("EXAMPLE_ACCESS", "EXAMPLE_SECRET").unwrap())
        .with_metadata_cache(Some(Arc::new(Cache::new(f.dir.join("proofs")))))
        .test_endpoint(&f.endpoint);
    let blob = Blob {
        id: sha256(bytes),
        size: bytes.len() as u64,
        remote: RemoteLocation { connection: "personal".into(), key: "library/original".into(), version: None },
    };
    (f, c, blob)
}
fn costs(f: &Fixture) -> (usize, usize, u64) {
    let state = f.state.lock().unwrap();
    assert_eq!(state.signature_errors, 0);
    (
        state.requests.iter().filter(|(m, _)| m == "GET").count(),
        state.requests.iter().filter(|(m, _)| m == "HEAD").count(),
        state.traffic.iter().filter(|r| r.method == "GET").map(|r| r.response_bytes).sum(),
    )
}
fn admit(c: &S3Connection, b: &Blob, proof: Option<UploadEvidence>) -> Result<VerifiedMedia> {
    let mut media = VerifiedMedia::default();
    let cancel = AtomicBool::new(false);
    media.admit_upload(c, b, proof, &cancel)?;
    if !media.verify(c, b, &cancel, &mut |_| {})? {
        return Err(AssetError::Missing);
    }
    Ok(media)
}
fn upload(c: &S3Connection, b: &Blob, bytes: &[u8]) -> Result<Option<UploadEvidence>> {
    c.put_blob_checksum(b, &mut Cursor::new(bytes), &AtomicBool::new(false), &mut |_| {}, false)
}
fn multipart(c: &S3Connection, b: &Blob, bytes: &[u8]) -> Result<Option<UploadEvidence>> {
    let cancel = AtomicBool::new(false);
    let (id, checksum) = c.start_multipart_checksum(b, &cancel)?;
    let mut parts = Vec::new();
    let mut full = Sha256::new();
    for (index, bytes) in bytes.chunks(PART_BYTES).enumerate() {
        let number = u16::try_from(index + 1).unwrap();
        full.update(bytes);
        let (etag, proof) = c.upload_part_checksum(b, PartRequest { upload_id: &id, number, checksum }, bytes, &cancel, &mut |_| {})?;
        parts.push(ChecksumPart { number, etag, hash: sha256(bytes), size: bytes.len() as u64, checksum: proof });
    }
    c.complete_multipart_checksum(
        b,
        MultipartCompletion { upload_id: &id, parts: &parts, source_hash: &BlobId(format!("{:x}", full.finalize())), checksum, immutable: false },
        &cancel,
    )
}
#[test]
fn aws_single_upload_uses_validated_sha256_and_exact_version_without_media_readback() {
    assert_eq!(part_checksum(&sha256(DATA)).unwrap(), "ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=");
    let (f, c, b) = fixture(DATA);
    let proof = upload(&c, &b, DATA).unwrap();
    assert!(proof.is_some());
    assert!(c.media_receipt(&b, &AtomicBool::new(false)).unwrap().is_none(), "headers are not publication admission");
    let media = admit(&c, &b, proof).unwrap();
    assert!(media.contains(&c, &b));
    assert_eq!(costs(&f), (0, 1, 0));
    drop(media);
    assert!(VerifiedMedia::default().verify(&c, &b, &AtomicBool::new(false), &mut |_| {}).unwrap());
    assert_eq!(costs(&f), (0, 2, 0));
}
#[test]
fn aws_multipart_checks_source_parts_composite_and_version_without_bulk_gets() {
    let bytes: Vec<_> = (0..(9 << 20)).map(|n| (n % 251) as u8).collect();
    let (f, c, b) = fixture(&bytes);
    let proof = multipart(&c, &b, &bytes).unwrap();
    assert!(proof.is_some());
    admit(&c, &b, proof).unwrap();
    assert_eq!(costs(&f), (0, 1, 0));
    let state = f.state.lock().unwrap();
    assert_eq!(state.created, 1);
    assert_eq!(state.objects[PATH], bytes);
    assert!(state.multipart.is_empty());
    assert_eq!(state.requests.iter().filter(|(m, p)| m == "PUT" && p.contains("partNumber=")).count(), 2);
}
#[test]
fn omitted_provider_evidence_and_user_metadata_fall_back_to_full_verification() {
    for fault in ["missing-final", "metadata-only"] {
        let (f, c, b) = fixture(DATA);
        f.state.lock().unwrap().checksum_fault = Some(fault.into());
        let proof = upload(&c, &b, DATA).unwrap();
        assert!(proof.is_none(), "{fault}");
        admit(&c, &b, proof).unwrap();
        assert_eq!(costs(&f), (1, 0, 3));
    }
    for fault in ["missing-init", "missing-part", "missing-final", "metadata-only"] {
        let (f, c, b) = fixture(DATA);
        f.state.lock().unwrap().checksum_fault = Some(fault.into());
        let proof = multipart(&c, &b, DATA).unwrap();
        assert!(proof.is_none(), "{fault}");
        admit(&c, &b, proof).unwrap();
        assert_eq!(costs(&f), (1, 0, 3));
    }
}
#[test]
fn contradictory_checksums_types_and_completion_bindings_never_create_evidence() {
    for fault in ["wrong", "wrong-type"] {
        let (f, c, b) = fixture(DATA);
        f.state.lock().unwrap().checksum_fault = Some(fault.into());
        assert!(matches!(upload(&c, &b, DATA), Err(AssetError::Integrity)), "{fault}");
        assert!(c.media_receipt(&b, &AtomicBool::new(false)).unwrap().is_none());
        assert_eq!(costs(&f), (0, 0, 0));
    }
    for fault in ["wrong", "wrong-final", "wrong-type", "wrong-key"] {
        let (f, c, b) = fixture(DATA);
        f.state.lock().unwrap().checksum_fault = Some(fault.into());
        assert!(matches!(multipart(&c, &b, DATA), Err(AssetError::Integrity)), "{fault}");
        assert!(c.media_receipt(&b, &AtomicBool::new(false)).unwrap().is_none());
        assert_eq!(costs(&f), (0, 0, 0));
    }
}
#[test]
fn mismatched_response_version_or_replacement_requires_remote_bytes() {
    let (f, c, b) = fixture(DATA);
    f.state.lock().unwrap().checksum_fault = Some("wrong-version".into());
    let proof = upload(&c, &b, DATA).unwrap();
    admit(&c, &b, proof).unwrap();
    assert_eq!(costs(&f), (1, 1, 3));
    let (f, c, b) = fixture(DATA);
    let proof = upload(&c, &b, DATA).unwrap();
    {
        let mut state = f.state.lock().unwrap();
        state.media_versions.insert(PATH.into(), "version-two".into());
        state.objects.insert(PATH.into(), b"bad".to_vec());
    }
    assert!(matches!(admit(&c, &b, proof), Err(AssetError::Integrity)));
    assert_eq!(costs(&f), (1, 1, 3));
    assert!(c.media_receipt(&b, &AtomicBool::new(false)).unwrap().is_none());
}
#[test]
fn versionless_unknown_and_b2_endpoints_never_infer_checksum_capability() {
    let (f, c, b) = fixture(DATA);
    f.state.lock().unwrap().media_versions.clear();
    let proof = upload(&c, &b, DATA).unwrap();
    assert!(proof.is_none());
    admit(&c, &b, proof).unwrap();
    assert_eq!(costs(&f), (1, 0, 3));
    for endpoint in ["local", "b2"] {
        let (f, mut c, b) = fixture(DATA);
        c.endpoint = Some(if endpoint == "b2" { "https://s3.us-east-005.backblazeb2.com".into() } else { f.endpoint.clone() });
        if endpoint == "b2" {
            c.region = "us-east-005".into();
        }
        let proof = upload(&c, &b, DATA).unwrap();
        assert!(proof.is_none());
        admit(&c, &b, proof).unwrap();
        assert_eq!(costs(&f), (1, 0, 3));
    }
}
#[test]
fn uncertain_put_or_completion_and_explicit_integrity_checks_always_read_back() {
    let (f, c, b) = fixture(DATA);
    f.state.lock().unwrap().lost_put = true;
    assert!(upload(&c, &b, DATA).err().unwrap().uncertain_write());
    assert!(VerifiedMedia::default().verify(&c, &b, &AtomicBool::new(false), &mut |_| {}).unwrap());
    assert_eq!(costs(&f), (1, 0, 3));
    let (f, c, b) = fixture(DATA);
    f.state.lock().unwrap().lost_complete = true;
    assert!(multipart(&c, &b, DATA).err().unwrap().uncertain_write());
    assert!(VerifiedMedia::default().verify(&c, &b, &AtomicBool::new(false), &mut |_| {}).unwrap());
    assert_eq!(costs(&f), (1, 0, 3));
    let (f, c, b) = fixture(DATA);
    let proof = upload(&c, &b, DATA).unwrap();
    let mut media = VerifiedMedia::coordinated(Arc::new(SharedMedia::default()), 1, None, true);
    media.admit_upload(&c, &b, proof, &AtomicBool::new(false)).unwrap();
    assert!(media.verify(&c, &b, &AtomicBool::new(false), &mut |_| {}).unwrap());
    assert_eq!(costs(&f), (1, 0, 3));
}
#[test]
fn provider_rejects_bad_checksum_and_local_source_hash_cannot_be_forged_by_a_part_list() {
    let (f, c, b) = fixture(DATA);
    let headers = [("x-amz-checksum-sha256", "wrong".into())];
    let response = c
        .request(
            Request {
                method: Method::Put,
                key: &b.remote.key,
                query: &[],
                headers: &headers,
                payload: &b.id.0,
                body: Some(RequestBody { reader: &mut Cursor::new(DATA), length: 3 }),
            },
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap();
    assert_eq!(response.status, 400);
    assert!(!f.state.lock().unwrap().objects.contains_key(PATH));
    let cancel = AtomicBool::new(false);
    let (id, checksum) = c.start_multipart_checksum(&b, &cancel).unwrap();
    let (etag, proof) = c.upload_part_checksum(&b, PartRequest { upload_id: &id, number: 1, checksum }, DATA, &cancel, &mut |_| {}).unwrap();
    let parts = [ChecksumPart { number: 1, etag, hash: b.id.clone(), size: 3, checksum: proof }];
    assert_eq!(composite(&parts).unwrap(), "T4tCwi3TcptRm6b2jS2nzFstYG0F2u1a1RKMwD5sY1g=-1");
    let before = f.state.lock().unwrap().requests.len();
    assert!(matches!(
        c.complete_multipart_checksum(
            &b,
            MultipartCompletion { upload_id: &id, parts: &parts, source_hash: &sha256(b"bad"), checksum, immutable: false },
            &cancel
        ),
        Err(AssetError::Integrity)
    ));
    assert_eq!(f.state.lock().unwrap().requests.len(), before);
    assert!(!f.state.lock().unwrap().objects.contains_key(PATH));
}
#[test]
fn corrupted_payload_and_embedded_completion_error_leave_no_verification_receipt() {
    let (f, c, b) = fixture(DATA);
    assert!(upload(&c, &b, b"bad").is_err());
    assert!(!f.state.lock().unwrap().objects.contains_key(PATH));
    assert!(c.media_receipt(&b, &AtomicBool::new(false)).unwrap().is_none());
    let (f, c, b) = fixture(DATA);
    f.state.lock().unwrap().embedded_error = true;
    assert!(multipart(&c, &b, DATA).is_err());
    assert!(!f.state.lock().unwrap().objects.contains_key(PATH));
    assert!(c.media_receipt(&b, &AtomicBool::new(false)).unwrap().is_none());
}
