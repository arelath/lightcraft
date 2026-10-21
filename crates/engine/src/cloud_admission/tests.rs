use super::*;
use crate::assets::{
    AssetResolver, BlobStore, SourceRequirement, TransferManager,
    s3::{AccessKeys, S3Connection, fixture::Fixture},
};
use lightcraft_catalog::assets::{Asset, Blob, RemoteLocation};
use serde_json::json;
fn fixture() -> (Fixture, S3Connection, Blob) {
    let f = Fixture::new();
    f.state.lock().unwrap().allow_typed_keys = true;
    let c: S3Connection =
        serde_json::from_value(json!({"id":"personal","bucket":"photos","region":"us-east-1","prefix":"library","endpoint":f.endpoint})).unwrap();
    let c = c.with_keys(AccessKeys::new("EXAMPLE_ACCESS", "EXAMPLE_SECRET").unwrap());
    let b = Blob {
        id: crate::assets::sha256(b"original"),
        size: 8,
        remote: RemoteLocation { connection: "personal".into(), key: "library/original".into(), version: None },
    };
    (f, c, b)
}
#[test]
fn cap_refusal_suspends_s3_metadata_media_and_other_buckets_until_explicit_retry() {
    let (f, c, b) = fixture();
    let cancel = AtomicBool::new(false);
    f.state.lock().unwrap().download_cap = true;
    assert_eq!(c.get_small("library/refused", 1024, &cancel).unwrap_err(), AssetError::ProviderLimit { status: 403 });
    let mut other = c.clone();
    other.bucket = "another-bucket".into();
    assert_eq!(other.stat_object("library/untouched", &cancel).unwrap_err(), AssetError::ProviderLimit { status: 403 });
    assert_eq!(c.download(&b, &mut Vec::new(), &cancel, &mut |_| {}).unwrap_err(), AssetError::ProviderLimit { status: 403 });
    assert_eq!(
        c.put_small("library/untouched", b"x", crate::assets::s3::PutCondition::Absent, &cancel).unwrap_err(),
        AssetError::ProviderLimit { status: 403 }
    );
    assert_eq!(f.state.lock().unwrap().requests.len(), 1);
    assert_eq!(c.check_admission(&AtomicBool::new(true)), Err(AssetError::Cancelled));
    f.state.lock().unwrap().download_cap = false;
    c.retry_admission().unwrap();
    assert!(c.get_small("library/refused", 1024, &cancel).unwrap().is_none());
    assert_eq!(f.state.lock().unwrap().requests.len(), 2);
    assert!(Scope::new("different-provider", "EXAMPLE_ACCESS", "EXAMPLE_SECRET").check(&cancel).is_ok());
    assert!(Scope::new(&f.endpoint, "different-key", "EXAMPLE_SECRET").check(&cancel).is_ok());
}
#[test]
fn throttling_is_shared_preserves_retry_after_and_explicit_retry_cannot_bypass_it() {
    let (f, c, b) = fixture();
    let cancel = AtomicBool::new(false);
    {
        let mut state = f.state.lock().unwrap();
        state.reject_status = Some(503);
        state.retry_after = Some("60".into());
    }
    assert_eq!(c.get_small("library/refused", 1024, &cancel).unwrap_err(), AssetError::Throttled { status: 503, retry_after_ms: 60_000 });
    c.retry_admission().unwrap();
    assert!(matches!(c.download(&b, &mut Vec::new(), &cancel, &mut |_| {}), Err(AssetError::Throttled { status: 503, retry_after_ms: 1..=60_000 })));
    assert_eq!(f.state.lock().unwrap().requests.len(), 1);
    for (header, expected) in [(Some("invalid"), 2000), (Some("18446744073709551615"), MAX_DELAY_MS), (Some("0"), 1000)] {
        let scope = Scope::new(header.unwrap(), "unique", "test");
        assert_eq!(scope.throttle(429, header), AssetError::Throttled { status: 429, retry_after_ms: expected });
        let failure = crate::cloud_status::CloudFailure::from(AssetError::Throttled { status: 429, retry_after_ms: expected });
        let mut budget = crate::cloud_status::RetryBudget::default();
        assert_eq!(budget.failed(&failure, Some(1000)), Some((expected / 1000).max(2)));
    }
}
#[test]
fn a_cap_does_not_trigger_original_fallback_but_cached_media_stays_usable() {
    let (f, c, original) = fixture();
    let cancel = AtomicBool::new(false);
    let preview = Blob {
        id: crate::assets::sha256(b"preview"),
        size: 7,
        remote: RemoteLocation { connection: "personal".into(), key: "library/preview".into(), version: None },
    };
    let asset = Asset { original: original.clone(), smart_preview: Some(preview), thumbnail: None, associated: Default::default() };
    let resolver = AssetResolver::new(f.dir.join("cache"));
    resolver.connect_s3(c.clone()).unwrap();
    f.state.lock().unwrap().download_cap = true;
    assert!(matches!(resolver.acquire_asset(&asset, SourceRequirement::PreviewAllowed, &cancel), Err(AssetError::ProviderLimit { .. })));
    assert_eq!(f.state.lock().unwrap().requests.len(), 1, "preview cap is not followed by original GET");
    f.state.lock().unwrap().download_cap = false;
    c.retry_admission().unwrap();
    f.state.lock().unwrap().objects.insert("/photos/library/original".into(), b"original".to_vec());
    drop(resolver.acquire(&original, &cancel).unwrap());
    Scope::new(&f.endpoint, "EXAMPLE_ACCESS", "EXAMPLE_SECRET").cap(403);
    assert!(resolver.acquire_asset(&asset, SourceRequirement::PreviewAllowed, &cancel).is_ok());
    assert_eq!(f.state.lock().unwrap().requests.len(), 2);
}
#[test]
fn ordinary_forbidden_responses_do_not_suspend_other_requests() {
    let (f, c, _) = fixture();
    let cancel = AtomicBool::new(false);
    f.state.lock().unwrap().reject_status = Some(403);
    assert_eq!(c.get_small("library/a", 1024, &cancel).unwrap_err(), AssetError::Http(403));
    assert_eq!(c.get_small("library/b", 1024, &cancel).unwrap_err(), AssetError::Http(403));
    assert_eq!(f.state.lock().unwrap().requests.len(), 2);
}
