use super::*;
use lightcraft_fetch::http::{HttpError, Limits, Method, RequestBody, Url};
use std::{
    io::{Cursor, Read, Write},
    net::TcpListener,
    sync::atomic::AtomicBool,
};

fn server(status: u16, advertised: usize, bytes: Vec<u8>) -> (Url, std::thread::JoinHandle<usize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = Url::parse(&format!("http://{}/private-object?signature=do-not-log", listener.local_addr().unwrap())).unwrap();
    let worker = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        stream.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            assert!(header.len() < 32 << 10);
            let mut byte = [0];
            stream.read_exact(&mut byte).unwrap();
            header.push(byte[0]);
        }
        let header = String::from_utf8(header).unwrap();
        let size = header
            .lines()
            .filter_map(|l| l.split_once(':'))
            .find(|(key, _)| key.eq_ignore_ascii_case("content-length"))
            .map_or(0, |(_, v)| v.trim().parse::<usize>().unwrap());
        let mut request = vec![0; size];
        let mut received = 0;
        while received < size {
            match stream.read(&mut request[received..]) {
                Ok(0) | Err(_) => return received,
                Ok(n) => received += n,
            }
        }
        write!(stream, "HTTP/1.1 {status} Fixture\r\nContent-Length: {advertised}\r\nConnection: close\r\n\r\n").unwrap();
        stream.write_all(&bytes).unwrap();
        size
    });
    (url, worker)
}
fn totals(scope: &Scope) -> Totals {
    let id = scope.operation.unwrap();
    registry().lock().unwrap().operations.iter().find(|o| o.id == id).unwrap().totals.clone()
}
fn records(scope: &Scope) -> Vec<serde_json::Value> {
    let id = scope.operation.unwrap();
    registry().lock().unwrap().recent.iter().filter(|r| r.operation_path.contains(&id)).map(|r| serde_json::to_value(r).unwrap()).collect()
}
fn drain(response: &mut Response, limits: &Limits) -> Result<(), HttpError> {
    let mut buffer = [0; 8192];
    while response.read(&mut buffer, limits)? != 0 {}
    Ok(())
}

#[test]
fn streamed_upload_and_media_bytes_match_the_backend_and_scope_without_secret_fields() {
    let background = Scope::background_poll(true);
    let parent = Scope::operation(Purpose::Sync, Some("private-library"), None, Some(3));
    let child = Scope::operation(Purpose::UploadVerification, None, Some("private-import"), None);
    let cancel = AtomicBool::new(false);
    let limits = Limits { connect: Duration::from_secs(2), stall: Duration::from_secs(2), cancel: &cancel };
    let data = vec![42; 70_000];
    let mut source = Cursor::new(&data);
    let (url, server) = server(200, 2, b"ok".to_vec());
    let mut response = request(
        Target { destination: "destination-digest", resource: &url.path, transport: "s3", kind: "object", method: Method::Put },
        &url,
        &[("authorization", "never-log-this".into())],
        Some(RequestBody { reader: &mut source, length: data.len() as u64 }),
        &limits,
        &mut |_| {},
    )
    .unwrap();
    drain(&mut response, &limits).unwrap();
    drop(response);
    assert_eq!(server.join().unwrap(), data.len());
    let (url, server) = self::server(200, data.len(), data.clone());
    let mut response = request(
        Target { destination: "destination-digest", resource: &url.path, transport: "s3Media", kind: "media", method: Method::Get },
        &url,
        &[],
        None,
        &limits,
        &mut |_| {},
    )
    .unwrap();
    drain(&mut response, &limits).unwrap();
    drop(response);
    assert_eq!(server.join().unwrap(), 0);
    reuse(false);
    reuse(true);
    let count = totals(&child);
    assert_eq!((count.attempts, count.responses, count.body_bytes_sent, count.body_bytes_read), (2, 2, 70_000, 70_002));
    assert_eq!((count.complete_bodies, count.background_poll_attempts, count.background_poll_body_bytes_read), (2, 2, 70_002));
    assert_eq!((count.local_cache_hits, count.shared_proof_hits), (1, 1));
    assert_eq!(totals(&parent).body_bytes_read, 70_002, "parent includes the child once; global totals never add an extra request for grouping");
    let serialized = serde_json::to_string(&records(&child)).unwrap();
    for secret in ["private-library", "private-import", "never-log-this", "do-not-log", "signature=", "private-object"] {
        assert!(!serialized.contains(secret), "{secret}");
    }
    assert!(serialized.contains("uploadVerification"));
    assert!(records(&child).iter().all(|r| r["backgroundPoll"] == true));
    drop(child);
    drop(parent);
    drop(background);
}

#[test]
fn truncated_refused_cancelled_and_dropped_responses_keep_exact_read_lower_bounds() {
    let scope = Scope::operation(Purpose::Direct, None, None, None);
    let cancel = AtomicBool::new(false);
    let limits = Limits { connect: Duration::from_secs(2), stall: Duration::from_secs(2), cancel: &cancel };
    let (url, server) = server(200, 100, b"partial".to_vec());
    let mut response = request(
        Target { destination: "partial", resource: &url.path, transport: "s3Media", kind: "media", method: Method::Get },
        &url,
        &[],
        None,
        &limits,
        &mut |_| {},
    )
    .unwrap();
    assert!(drain(&mut response, &limits).is_err());
    drop(response);
    server.join().unwrap();
    assert_eq!((totals(&scope).body_bytes_read, totals(&scope).transport_failures), (7, 1));
    let (url, server) = self::server(403, 3, b"cap".to_vec());
    let mut response = request(
        Target { destination: "partial", resource: &url.path, transport: "s3", kind: "object", method: Method::Get },
        &url,
        &[],
        None,
        &limits,
        &mut |_| {},
    )
    .unwrap();
    drain(&mut response, &limits).unwrap();
    response.provider_cap();
    drop(response);
    server.join().unwrap();
    assert_eq!((totals(&scope).body_bytes_read, totals(&scope).provider_caps, totals(&scope).http_errors), (10, 1, 1));
    let (url, server) = self::server(200, 6, b"unused".to_vec());
    let response = request(
        Target { destination: "partial", resource: &url.path, transport: "s3", kind: "object", method: Method::Get },
        &url,
        &[],
        None,
        &limits,
        &mut |_| {},
    )
    .unwrap();
    drop(response);
    server.join().unwrap();
    assert_eq!(
        (totals(&scope).body_bytes_read, totals(&scope).complete_bodies),
        (10, 1),
        "dropping unread prefetched data never claims a complete transfer"
    );
    let cancelled = AtomicBool::new(true);
    let limits = Limits { connect: Duration::from_secs(1), stall: Duration::from_secs(1), cancel: &cancelled };
    assert!(
        request(
            Target { destination: "partial", resource: &url.path, transport: "s3", kind: "object", method: Method::Get },
            &url,
            &[],
            None,
            &limits,
            &mut |_| {}
        )
        .is_err()
    );
    let last = records(&scope).pop().unwrap();
    assert_eq!(last["status"], serde_json::Value::Null);
    assert_eq!(last["transportFailure"], true);
    assert_eq!(totals(&scope).attempts, 4);
}

#[test]
fn bounded_registry_keeps_lifetime_totals_and_does_not_resurrect_old_hours() {
    let mut registry = Registry { start: Instant::now().checked_sub(Duration::from_secs(30 * 3600)).unwrap(), ..Default::default() };
    let context = Context::default();
    for n in 0..1000 {
        registry.operation(&context);
        registry.add(&context, Some(&format!("destination-{n}")), 30, &Totals { attempts: 1, ..Default::default() });
    }
    registry.add(&context, Some("older"), 0, &Totals { body_bytes_read: 17, ..Default::default() });
    assert_eq!(registry.totals.attempts, 1000);
    assert_eq!(registry.totals.body_bytes_read, 17);
    assert_eq!(registry.operations.len(), MAX_OPERATIONS);
    assert_eq!(registry.operations.front().unwrap().id, 1, "an active operation cannot lose its cost totals to detail eviction");
    assert!(registry.untracked_operations > 0);
    assert!(registry.destinations.len() <= MAX_DESTINATIONS + 1);
    assert!(registry.hours.iter().all(|h| h.hour_since_start >= 7));
    assert_eq!(registry.hours.iter().map(|h| h.totals.body_bytes_read).sum::<u64>(), 0);
}

#[test]
fn destination_binding_ignores_keys_but_tracks_endpoint_bucket_and_prefix() {
    let base: S3Connection = serde_json::from_value(serde_json::json!({"id":"test","bucket":"photos","region":"us-east-1","prefix":"library/","endpoint":"https://storage.example/","profile":"one"})).unwrap();
    let mut renamed = base.clone();
    renamed.profile = Some("two".into());
    renamed.prefix = "library".into();
    renamed.endpoint = Some("https://storage.example".into());
    assert_eq!(destination(&base), destination(&renamed));
    assert_eq!(
        destination(&base),
        destination(&base.clone().with_keys(crate::assets::s3::AccessKeys::new("EXAMPLE_ACCESS", "EXAMPLE_SECRET").unwrap()))
    );
    renamed.bucket = "other".into();
    assert_ne!(destination(&base), destination(&renamed));
    renamed = base.clone();
    renamed.prefix = "different".into();
    assert_ne!(destination(&base), destination(&renamed));
}

#[test]
fn helper_thread_attribution_preserves_poll_library_and_operation_and_restores_its_context() {
    let background = Scope::background_poll(true);
    let scope = Scope::operation(Purpose::Sync, Some("owned-library"), None, Some(23));
    let attribution = Attribution::capture();
    let (url, server) = server(200, 2, b"ok".to_vec());
    std::thread::spawn(move || {
        {
            let _owner = attribution.enter();
            let _audit = Scope::purpose(Purpose::Audit);
            let cancel = AtomicBool::new(false);
            let limits = Limits { connect: Duration::from_secs(2), stall: Duration::from_secs(2), cancel: &cancel };
            let mut response = request(
                Target { destination: "thread", resource: &url.path, transport: "s3", kind: "object", method: Method::Get },
                &url,
                &[],
                None,
                &limits,
                &mut |_| {},
            )
            .unwrap();
            drain(&mut response, &limits).unwrap();
        }
        CONTEXT.with(|context| {
            assert!(context.borrow().path.is_empty());
            assert!(!context.borrow().background_poll);
        });
    })
    .join()
    .unwrap();
    server.join().unwrap();
    assert_eq!(totals(&scope).attempts, 1);
    assert_eq!(totals(&scope).background_poll_attempts, 1);
    assert_eq!(records(&scope)[0]["purpose"], "audit");
    assert_eq!(records(&scope)[0]["library"], identity("owned-library"));
    drop(scope);
    drop(background);
    CONTEXT.with(|context| assert!(!context.borrow().background_poll));
}

#[test]
fn a_request_source_failure_keeps_confirmed_outbound_bytes() {
    struct BrokenSource(usize);
    impl Read for BrokenSource {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            if self.0 == 0 {
                return Err(std::io::Error::other("synthetic source failure"));
            }
            let n = self.0.min(output.len());
            output[..n].fill(42);
            self.0 -= n;
            Ok(n)
        }
    }
    let scope = Scope::operation(Purpose::Upload, None, None, None);
    let (url, server) = server(200, 0, Vec::new());
    let cancel = AtomicBool::new(false);
    let limits = Limits { connect: Duration::from_secs(2), stall: Duration::from_secs(2), cancel: &cancel };
    let mut source = BrokenSource(32_000);
    assert!(
        request(
            Target { destination: "broken-source", resource: &url.path, transport: "s3", kind: "object", method: Method::Put },
            &url,
            &[],
            Some(RequestBody { reader: &mut source, length: 70_000 }),
            &limits,
            &mut |_| {}
        )
        .is_err()
    );
    assert_eq!(server.join().unwrap(), 32_000);
    assert_eq!((totals(&scope).body_bytes_sent, totals(&scope).transport_failures, totals(&scope).finished_attempts), (32_000, 1, 1));
    assert_eq!(records(&scope)[0]["status"], serde_json::Value::Null);
}

#[test]
fn diagnostic_parameters_and_operation_selection_are_read_only() {
    let scope = Scope::operation(Purpose::Setup, Some("library"), None, None);
    let mut session = crate::Session::new();
    for params in [
        serde_json::json!(false),
        serde_json::json!([]),
        serde_json::json!({"limit":-1}),
        serde_json::json!({"limit":257}),
        serde_json::json!({"limit":"many"}),
        serde_json::json!({"operation":u64::MAX}),
    ] {
        assert!(matches!(status(&mut session, &params), Err(crate::EngineError::BadParams { .. })));
    }
    reuse(false);
    let before = totals(&scope);
    let report = status(&mut session, &serde_json::json!({"limit":0,"operation":scope.operation.unwrap()})).unwrap();
    assert_eq!(report["totalsScope"], "operation");
    assert_eq!(report["totals"]["attempts"], 0);
    assert_eq!(report["totals"]["localCacheHits"], 1);
    assert!(report["recentRequests"].as_array().unwrap().is_empty());
    assert_eq!(serde_json::to_value(before).unwrap(), serde_json::to_value(totals(&scope)).unwrap());
}
