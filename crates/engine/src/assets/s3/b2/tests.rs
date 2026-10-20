use super::*;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::Ordering;
mod integration;

fn connection() -> S3Connection {
    serde_json::from_value(json!({"id":"personal","bucket":"photos-test","region":"us-east-005","prefix":"library/a","endpoint":"https://s3.us-east-005.backblazeb2.com"})).unwrap()
}
fn reference(scope: &Scope) -> RecordRef {
    RecordRef {
        key: format!("{}/authority/{}.json", scope.root, "a".repeat(64)),
        hash: lightcraft_catalog::assets::BlobId("a".repeat(64)),
        size: 128,
    }
}

#[test]
fn authority_pointers_bind_storage_prefix_and_are_small_and_canonical() {
    let c = connection();
    let a = Scope::new(&c).unwrap();
    let mut other = c.clone();
    other.prefix = "library/b".into();
    let b = Scope::new(&other).unwrap();
    assert_ne!(a.entry, b.entry);
    assert!(a.entry.len() <= 50);
    let p = a.pointer(&reference(&a)).unwrap();
    assert_eq!(a.decode(Some(&p)).unwrap(), Some(reference(&a)));
    assert!(b.decode(Some(&p)).is_err());
    let mut bad = reference(&a);
    bad.key = "other/authority/a.json".into();
    assert!(a.pointer(&bad).is_err());
    bad = reference(&a);
    bad.size = MAX_AUTHORITY + 1;
    assert!(a.pointer(&bad).is_err());
    let mut renamed = c.clone();
    renamed.id = "another-local-name".into();
    renamed.profile = Some("other-key".into());
    assert_eq!(a.binding, Scope::new(&renamed).unwrap().binding);
    renamed.prefix.push('/');
    assert_eq!(a.binding, Scope::new(&renamed).unwrap().binding);
    renamed.prefix.clear();
    assert!(Scope::new(&renamed).is_err());
    renamed = c;
    renamed.region = "us-west-004".into();
    assert!(Scope::new(&renamed).is_err());
    let mut pointer: Value = serde_json::from_str(p.as_str().unwrap()).unwrap();
    pointer["version"] = json!(2);
    assert!(matches!(a.decode(Some(&json!(pointer.to_string()))), Err(AssetError::Blocked(crate::cloud_control::Blocked::UpdateToRead(_)))));
    pointer["version"] = json!(1);
    let spaced = serde_json::to_string_pretty(&pointer).unwrap();
    assert!(a.decode(Some(&json!(spaced))).is_err());
}

#[test]
fn custom_region_like_hosts_never_select_native_b2_authority() {
    let mut c = connection();
    c.endpoint = Some("https://us1".into());
    c.region = "us1".into();
    c.validate().unwrap();
    assert!(Scope::new(&c).is_err());
    assert!(!c.b2_head_enabled(&AtomicBool::new(false)).unwrap(), "no request to a custom endpoint or the B2 account API");
    assert!(matches!(c.b2_read_authority(&AtomicBool::new(false)), Err(AssetError::Unconfigured(_))));
}

#[test]
fn arbitrary_bucket_info_values_are_preserved_and_capacity_is_bounded() {
    let mut info = Map::new();
    info.insert("unrelated".into(), json!({"nested":[1,true,"text"]}));
    assert!(bounded_info(&info).is_ok());
    let old = info.clone();
    let scope = Scope::new(&connection()).unwrap();
    info.insert(scope.entry.clone(), scope.pointer(&reference(&scope)).unwrap());
    assert_eq!(info.get("unrelated"), old.get("unrelated"));
    info.insert("huge".into(), Value::String("x".repeat(MAX_INFO)));
    assert!(matches!(bounded_info(&info), Err(AssetError::Capacity(_))));
}

#[test]
fn returned_api_endpoints_cannot_redirect_native_credentials() {
    for raw in [
        "http://api005.backblazeb2.com",
        "https://evil.example",
        "https://api005.backblazeb2.com.evil.example",
        "https://api.evil.backblazeb2.com",
        "https://api005.backblazeb2.com/path",
        "https://api005.backblazeb2.com?token=x",
        "https://api005.backblazeb2.com#fragment",
        "https://api005.backblazeb2.com:444",
        "https://user@api005.backblazeb2.com",
    ] {
        assert!(endpoint(raw).is_err(), "{raw}");
    }
    assert_eq!(endpoint("https://api005.backblazeb2.com").unwrap(), "https://api005.backblazeb2.com");
}

#[test]
fn basic_uses_rfc4648_padding_without_serializing_keys() {
    for (access, secret, expected) in [("f", "o", "Basic Zjpv"), ("fo", "o", "Basic Zm86bw=="), ("foo", "ba", "Basic Zm9vOmJh")] {
        let c = Credentials::checked(access.into(), secret.into(), None).unwrap();
        assert_eq!(basic(&c).unwrap(), expected);
    }
}

#[derive(Default)]
struct ServerState {
    info: Map<String, Value>,
    revision: u64,
    writes: usize,
    fault: Option<u16>,
    lose_reply: bool,
    malformed_reply: bool,
}
struct Server {
    api: String,
    state: Arc<Mutex<ServerState>>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Server {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let api = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let state = Arc::new(Mutex::new(ServerState {
            info: json!({"keep":{"values":[1,"yes",true]}}).as_object().unwrap().clone(),
            revision: 7,
            ..Default::default()
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let worker = std::thread::spawn({
            let state = state.clone();
            let stop = stop.clone();
            move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => Self::handle(stream, &state),
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(2)),
                        Err(e) => panic!("test server: {e}"),
                    }
                }
            }
        });
        Self { api, state, stop, worker: Some(worker) }
    }
    fn handle(mut stream: TcpStream, state: &Mutex<ServerState>) {
        stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        stream.set_write_timeout(Some(Duration::from_secs(3))).unwrap();
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            assert!(header.len() < 32 << 10);
            let mut byte = [0];
            stream.read_exact(&mut byte).unwrap();
            header.push(byte[0]);
        }
        let header = String::from_utf8(header).unwrap();
        assert!(header.to_ascii_lowercase().contains("authorization: fake-token\r\n"));
        let size =
            header.lines().find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length: ").map(|v| v.parse::<usize>().unwrap())).unwrap();
        assert!(size <= MAX_JSON);
        let mut body = vec![0; size];
        stream.read_exact(&mut body).unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        let mut state = state.lock().unwrap();
        let mut status = 200;
        let value = if let Some(code) = state.fault.take() {
            status = code;
            json!({"error":"synthetic refusal"})
        } else if header.starts_with("POST /b2api/v4/b2_list_buckets ") {
            assert_eq!(body, json!({"accountId":"account","bucketName":"photos-test","bucketTypes":["allPrivate","allPublic"]}));
            json!({"buckets":[{"accountId":"account","bucketId":"bucket-id","bucketName":"photos-test","revision":state.revision,"bucketInfo":state.info}]})
        } else {
            assert!(header.starts_with("POST /b2api/v4/b2_update_bucket "));
            let keys: Vec<_> = body.as_object().unwrap().keys().map(String::as_str).collect();
            assert_eq!(keys, ["accountId", "bucketId", "bucketInfo", "ifRevisionIs"]);
            assert_eq!(body["accountId"], "account");
            assert_eq!(body["bucketId"], "bucket-id");
            if body["ifRevisionIs"] != state.revision {
                status = 409;
            } else {
                state.writes += 1;
                state.revision += 1;
                state.info = body["bucketInfo"].as_object().unwrap().clone();
            }
            if state.lose_reply {
                state.lose_reply = false;
                return;
            }
            if state.malformed_reply {
                json!({"accountId":"different-account"})
            } else {
                json!({"accountId":"account","bucketId":"bucket-id","revision":state.revision,"bucketInfo":state.info})
            }
        };
        let bytes = serde_json::to_vec(&value).unwrap();
        write!(stream, "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len()).unwrap();
        let _ = stream.write_all(&bytes);
    }
    fn observed(&self) -> Observed {
        let scope = Scope::new(&connection()).unwrap();
        let state = self.state.lock().unwrap();
        Observed {
            revision: state.revision,
            head: None,
            scope: scope.binding,
            entry: scope.entry,
            bucket_id: "bucket-id".into(),
            info: state.info.clone(),
            auth: Arc::new(Auth { account: "account".into(), api: self.api.clone(), token: "fake-token".into(), writable: true }),
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

#[test]
fn competing_bucket_revisions_have_one_winner_and_preserve_other_settings() {
    let server = Server::new();
    let old = server.observed();
    let replies = std::thread::scope(|s| {
        let handles: Vec<_> = ["a", "b"]
            .into_iter()
            .map(|value| {
                let old = &old;
                s.spawn(move || {
                    let mut next = old.info.clone();
                    next.insert(old.entry.clone(), json!(value));
                    update(old, &next, &old.auth, &AtomicBool::new(false))
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect::<Vec<_>>()
    });
    assert_eq!(replies.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(replies.iter().filter(|r| matches!(r, Err(AssetError::Conflict))).count(), 1);
    let current = bucket(&connection(), &old.auth, &AtomicBool::new(false)).unwrap();
    assert_eq!(current["revision"], 8);
    assert_eq!(current["bucketInfo"]["keep"], old.info["keep"]);
    assert_eq!(server.state.lock().unwrap().writes, 1);
}

#[test]
fn lost_or_unconfirmed_updates_are_uncertain_and_must_be_reread() {
    for lost in [false, true] {
        let server = Server::new();
        let old = server.observed();
        let mut next = old.info.clone();
        next.insert(old.entry.clone(), json!("committed"));
        {
            let mut state = server.state.lock().unwrap();
            state.lose_reply = lost;
            state.malformed_reply = !lost;
        }
        let error = update(&old, &next, &old.auth, &AtomicBool::new(false)).unwrap_err();
        assert!(error.uncertain_write(), "{error}");
        let current = bucket(&connection(), &old.auth, &AtomicBool::new(false)).unwrap();
        assert_eq!(current["bucketInfo"], json!(next));
        assert_eq!(server.state.lock().unwrap().writes, 1);
    }
}

#[test]
fn redirects_authentication_and_cancellation_never_write_bucket_metadata() {
    let server = Server::new();
    let old = server.observed();
    for status in [301, 302, 303, 307, 308, 401, 403, 500] {
        server.state.lock().unwrap().fault = Some(status);
        let error = update(&old, &old.info, &old.auth, &AtomicBool::new(false)).unwrap_err();
        if (300..400).contains(&status) {
            assert!(matches!(error, AssetError::Unconfigured(_)));
        } else {
            assert!(matches!(error, AssetError::Http(code) if code == status));
        }
    }
    assert!(matches!(update(&old, &old.info, &old.auth, &AtomicBool::new(true)), Err(AssetError::Cancelled)));
    assert_eq!(server.state.lock().unwrap().writes, 0);
    let mut readonly = old;
    readonly.auth = Arc::new(Auth { account: "account".into(), api: server.api.clone(), token: "fake-token".into(), writable: false });
    let failure: crate::cloud_status::CloudFailure = readonly.require_writable().unwrap_err().into();
    assert_eq!(
        failure.remedy.unwrap_or_else(|| crate::cloud_status::Recovery::for_kind(failure.kind)),
        crate::cloud_status::Recovery::EditConnection
    );
    let c = connection();
    let reference = reference(&Scope::new(&c).unwrap());
    assert!(matches!(c.b2_compare_exchange_authority(&readonly, &reference, &AtomicBool::new(false)), Err(AssetError::Unconfigured(_))));
    let mut other = c;
    other.prefix = "another/library".into();
    assert!(matches!(other.b2_compare_exchange_authority(&readonly, &reference, &AtomicBool::new(false)), Err(AssetError::Invalid(_))));
}

#[test]
fn malformed_and_oversized_success_bodies_are_uncertain() {
    for oversized in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            stream.set_write_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                assert!(header.len() < 32 << 10);
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                header.push(byte[0]);
            }
            let length = if oversized { MAX_JSON + 1 } else { 5 };
            write!(stream, "HTTP/1.1 200 Test\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n{{bad!").unwrap();
        });
        let error = request(&url, "fake-token", None, &AtomicBool::new(false)).unwrap_err();
        assert!(error.uncertain_write(), "{error}");
        worker.join().unwrap();
    }
}

#[test]
fn authority_rejects_library_names_that_cannot_be_read_back() {
    let c = connection();
    let mut head = crate::cloud_records::Head::next(
        None,
        RecordRef {
            key: crate::cloud_records::object_key(&c.prefix, 1, "changes", &"a".repeat(64)),
            hash: lightcraft_catalog::assets::BlobId("a".repeat(64)),
            size: 1,
        },
        "valid-library",
        None,
        crate::cloud_control::Control::initial("valid-library"),
    )
    .unwrap();
    assert!(validate_head(&c, &head).is_ok());
    for id in ["invalid/library", "invalid.library"] {
        head.library = Some(id.into());
        head.control = Some(crate::cloud_control::Control::initial(id));
        assert!(head.validate_scope_typed(&c.prefix, id).is_ok(), "control text alone is insufficient");
        assert!(matches!(validate_head(&c, &head), Err(AssetError::Invalid(_))));
    }
}

/// Explicit opt-in against an authorized disposable bucket. Credentials resolve from
/// the selected existing profile and never enter test output or connection settings.
#[test]
#[ignore = "requires LIGHTCRAFT_B2_TEST_BUCKET and LIGHTCRAFT_B2_TEST_PROFILE for a disposable bucket"]
fn live_bucket_revision_cas_keeps_other_metadata_and_cleans_only_its_probe() {
    let bucket_name = std::env::var("LIGHTCRAFT_B2_TEST_BUCKET").expect("explicit disposable bucket");
    let profile = std::env::var("LIGHTCRAFT_B2_TEST_PROFILE").expect("explicit saved profile");
    let endpoint = std::env::var("LIGHTCRAFT_B2_TEST_ENDPOINT").expect("explicit B2 endpoint");
    let region = crate::cloud_setup::b2_endpoint(&endpoint).unwrap().1.to_owned();
    let token = crate::cloud_control::token().unwrap();
    let c: S3Connection = serde_json::from_value(json!({"id":"b2-cas-test","bucket":bucket_name,"region":region,"profile":profile,"endpoint":endpoint,"prefix":format!("library/cr43-native-cas-{token}")})).unwrap();
    let cancel = AtomicBool::new(false);
    let observed = c.b2_read_authority(&cancel).unwrap();
    assert!(observed.head().is_none());
    observed.require_writable().unwrap();
    let before = bucket(&c, &observed.auth, &cancel).unwrap();
    let scope = Scope::new(&c).unwrap();
    let library = format!("cr43-{token}");
    let mut objects = Vec::new();
    let check = (|| -> Result<()> {
        for digest in ["a", "b"] {
            let head = crate::cloud_records::Head::next(
                None,
                RecordRef {
                    key: crate::cloud_records::object_key(&c.prefix, 1, "changes", &digest.repeat(64)),
                    hash: lightcraft_catalog::assets::BlobId(digest.repeat(64)),
                    size: 1,
                },
                &library,
                None,
                crate::cloud_control::Control::initial(&library),
            )
            .map_err(|_| invalid("test head"))?;
            let bytes = serde_json::to_vec(&head).map_err(|_| invalid("test encoding"))?;
            let hash = sha256(&bytes);
            let reference = RecordRef { key: format!("{}/authority/{}.json", scope.root, hash.0), size: bytes.len() as u64, hash };
            objects.push(reference.clone());
            if c.b2_stage_head(&head, &cancel)? != reference {
                return Err(invalid("staged test head reference"));
            }
        }
        let outcomes = std::thread::scope(|threads| {
            let c = &c;
            let observed = &observed;
            let cancel = &cancel;
            let handles: Vec<_> = objects.iter().map(|next| threads.spawn(move || c.b2_compare_exchange_authority(observed, next, cancel))).collect();
            handles.into_iter().map(|handle| handle.join().map_err(|_| invalid("test worker"))).collect::<Result<Vec<_>>>()
        })?;
        if outcomes.iter().filter(|reply| reply.is_ok()).count() != 1
            || outcomes.iter().filter(|reply| matches!(reply, Err(AssetError::Conflict))).count() != 1
        {
            return Err(invalid("live CAS did not elect exactly one writer"));
        }
        let current = c.b2_read_authority(&cancel)?;
        if current.revision() <= observed.revision()
            || current.head().is_none_or(|head| !objects.contains(head))
            || current.info.get("keep") != observed.info.get("keep")
        {
            return Err(invalid("live committed pointer"));
        }
        for (key, value) in &observed.info {
            if current.info.get(key) != Some(value) {
                return Err(invalid("unrelated live bucket info changed"));
            }
        }
        Ok(())
    })();
    // Cleanup always runs, including after an uncertain response. Never remove an
    // unfamiliar pointer or any object outside this freshly generated exact scope.
    let cleanup = (|| -> Result<()> {
        let current = c.b2_read_authority(&cancel)?;
        if let Some(head) = current.head() {
            if !objects.contains(head) {
                return Err(invalid("probe scope changed; cleanup refused"));
            }
            let mut info = current.info.clone();
            info.remove(&current.entry);
            update(&current, &info, &current.auth, &cancel)?;
        }
        let current = c.b2_read_authority(&cancel)?;
        if current.head().is_some() {
            return Err(invalid("probe metadata cleanup not confirmed"));
        }
        let versions = c.library_versions(&cancel)?;
        if versions.iter().any(|version| !objects.iter().any(|object| object.key == version.key)) {
            return Err(invalid("unrecognized test object; cleanup refused"));
        }
        for version in versions {
            c.erase_version(&version, &cancel)?;
        }
        if !c.library_versions(&cancel)?.is_empty() {
            return Err(invalid("probe objects remain"));
        }
        let after = bucket(&c, &current.auth, &cancel)?;
        for property in
            ["bucketType", "corsRules", "lifecycleRules", "defaultServerSideEncryption", "fileLockConfiguration", "replicationConfiguration"]
        {
            if before.get(property) != after.get(property) {
                return Err(invalid("unrelated live bucket property changed"));
            }
        }
        Ok(())
    })();
    cleanup.unwrap();
    check.unwrap();
}
