//! Signed read-only discovery: complete pages, verified authorities, no guessed libraries.
#[path = "support/cloud_upload.rs"]
mod support;
use lightcraft_engine::{
    assets::s3::{AccessKeys, S3Connection},
    catalog::{assets::BlobId, replication::SyncProtocol},
    cloud_append::{self as append, control},
    cloud_control::{Control, State, Successor},
    cloud_publication::{Head, RecordRef},
    cloud_setup::{self, Probe},
};
use std::sync::atomic::AtomicBool;
use support::Fixture;
fn connection(f: &Fixture) -> S3Connection {
    f.state.lock().unwrap().allow_typed_keys = true;
    serde_json::from_value::<S3Connection>(
        serde_json::json!({"id":"personal","bucket":"photos","region":"us-east-1","prefix":"library","endpoint":f.endpoint}),
    )
    .unwrap()
    .with_keys(AccessKeys::new("EXAMPLE_ACCESS", "EXAMPLE_SECRET").unwrap())
}
fn probe(f: &Fixture) -> Result<Probe, String> {
    cloud_setup::probe(&connection(f), &AtomicBool::new(false))
}
fn put(f: &Fixture, key: &str, bytes: Vec<u8>) {
    f.state.lock().unwrap().objects.insert(format!("/photos/{key}"), bytes);
}
fn manifest(f: &Fixture, id: &str) {
    put(f, &append::manifest_key("library", id).unwrap(), append::manifest(id).unwrap());
}
fn control(f: &Fixture, c: Control) {
    put(f, &control::key("library", &c).unwrap(), control::encode(&c).unwrap());
}
fn head(f: &Fixture, id: &str, retired: bool) {
    let mut c = Control::initial(id);
    if retired {
        c.state = State::Retired;
        c.successor = Some(Successor {
            protocol: "append-log-v1".into(),
            epoch: 1,
            manifest: lightcraft_engine::assets::sha256(&append::manifest(id).unwrap()).0,
        });
    }
    let hash = "a".repeat(64);
    let head = Head::next(None, RecordRef { key: format!("library/catalog/changes/{hash}.json"), hash: BlobId(hash), size: 2 }, id, None, c).unwrap();
    put(f, "library/catalog/head.json", serde_json::to_vec(&head).unwrap());
}
fn readonly(f: &Fixture) {
    let state = f.state.lock().unwrap();
    assert!(state.requests.iter().all(|(method, _)| method == "GET"), "discovery must not write");
    assert_eq!(state.signature_errors, 0);
}
#[test]
fn empty_writable_bucket_is_ready_to_create_without_writing_during_probe() {
    let f = Fixture::new();
    assert_eq!(probe(&f).unwrap(), Probe::default());
    readonly(&f);
}
#[test]
fn cancelled_and_over_limit_discovery_never_offers_partial_libraries() {
    let f = Fixture::new();
    assert!(cloud_setup::probe(&connection(&f), &AtomicBool::new(true)).is_err());
    assert!(f.state.lock().unwrap().requests.is_empty());
    for n in 0..1025 {
        manifest(&f, &format!("library-{n:04}"));
    }
    assert!(probe(&f).unwrap_err().contains("Too many cloud libraries"));
    assert!(
        f.state.lock().unwrap().requests.iter().all(|(_, key)| key.contains("list-type=2")),
        "refuse the library count before reading manifests or a head"
    );
    readonly(&f);
}
#[test]
fn discovery_validates_every_library_beyond_the_first_listing_page() {
    let f = Fixture::new();
    f.state.lock().unwrap().list_page_size = 1;
    for id in ["a", "b", "c"] {
        manifest(&f, id);
    }
    let p = probe(&f).unwrap();
    assert_eq!(p.libraries.iter().map(|l| l.id.as_str()).collect::<Vec<_>>(), ["a", "b", "c"]);
    assert!(p.libraries.iter().all(|l| l.protocol == SyncProtocol::AppendLog));
    assert!(!p.head_taken);
    assert!(f.state.lock().unwrap().requests.iter().filter(|(_, path)| path.contains("continuation-token")).count() >= 2);
    readonly(&f);
}
#[test]
fn invalid_or_missing_manifest_and_unknown_root_objects_cannot_become_new_libraries() {
    for (key, bytes) in [
        ("library/sync-v2/not-valid!/manifest/library.json".into(), b"{}".to_vec()),
        (append::manifest_key("library", "a").unwrap(), b"{}".to_vec()),
        ("library/sync-v2/a/operations/placeholder.json".into(), b"{}".to_vec()),
        ("library/sync-v2/unknown-object".into(), b"{}".to_vec()),
    ] {
        let f = Fixture::new();
        put(&f, &key, bytes);
        assert!(probe(&f).is_err(), "{key}");
        readonly(&f);
    }
}
#[test]
fn a_failed_later_page_returns_no_partial_discovery() {
    let f = Fixture::new();
    f.state.lock().unwrap().list_page_size = 1;
    for id in ["a", "b"] {
        manifest(&f, id);
    }
    f.state.lock().unwrap().fail_list_after = Some(2);
    assert!(probe(&f).is_err());
    assert_eq!(f.state.lock().unwrap().list_calls, 2);
    readonly(&f);
}
#[test]
fn retired_head_resolves_only_to_a_validated_supported_successor() {
    let f = Fixture::new();
    manifest(&f, "a");
    head(&f, "a", true);
    let p = probe(&f).unwrap();
    assert!(p.head_taken);
    assert_eq!(p.libraries.len(), 1);
    assert_eq!(p.libraries[0].protocol, SyncProtocol::AppendLog);
    f.state.lock().unwrap().objects.remove(&format!("/photos/{}", append::manifest_key("library", "a").unwrap()));
    assert!(probe(&f).is_err());
    manifest(&f, "a");
    let key = "/photos/library/catalog/head.json";
    let mut h: serde_json::Value = serde_json::from_slice(&f.state.lock().unwrap().objects[key]).unwrap();
    h["control"]["successor"]["manifest"] = serde_json::json!("b".repeat(64));
    f.state.lock().unwrap().objects.insert(key.into(), serde_json::to_vec(&h).unwrap());
    assert!(probe(&f).is_err());
    readonly(&f);
}
#[test]
fn ambiguous_duplicate_identity_is_refused_but_distinct_protocol_libraries_are_preserved() {
    let f = Fixture::new();
    manifest(&f, "a");
    head(&f, "a", false);
    assert!(probe(&f).is_err());
    head(&f, "b", false);
    let p = probe(&f).unwrap();
    assert!(p.head_taken);
    assert_eq!(p.libraries.len(), 2);
    assert_eq!(p.libraries[0].id, "b");
    assert_eq!(p.libraries[0].protocol, SyncProtocol::ConditionalHead);
    assert_eq!(p.libraries[1].id, "a");
    readonly(&f);
}
#[test]
fn readable_newer_writer_is_found_but_unknown_namespace_deleted_and_conflicting_controls_refuse_join() {
    let f = Fixture::new();
    manifest(&f, "a");
    let mut c = Control::initial_append("a");
    c.minimum_writer = lightcraft_engine::cloud_control::WRITER + 1;
    c.schema = c.minimum_writer;
    c.revision = 2;
    control(&f, c.clone());
    assert_eq!(probe(&f).unwrap().libraries.len(), 1);
    c.epoch = 2;
    c.active_metadata_root = lightcraft_engine::cloud_control::append_root("a", 2);
    c.revision = 3;
    control(&f, c.clone());
    assert!(probe(&f).is_err());
    c = Control::initial_append("a");
    c.revision = 4;
    c.state = State::Deleted;
    control(&f, c.clone());
    assert!(probe(&f).is_err());
    c = Control::initial_append("a");
    c.revision = 5;
    control(&f, c.clone());
    c.minimum_writer += 1;
    c.schema = c.minimum_writer;
    control(&f, c);
    assert!(probe(&f).is_err());
    readonly(&f);
}
