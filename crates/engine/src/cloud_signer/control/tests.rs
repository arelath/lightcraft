use super::*;
use crate::cloud_records::{Head, RecordRef, b2_control, object_key};
use serde_json::{Value, json};
use std::net::Shutdown;

const ORIGIN: &str = "http://127.0.0.1:8080";
const TOKEN: &str = "EXAMPLE_SESSION_TOKEN_32_CHARACTERS";
fn config() -> Config {
    serde_json::from_value(json!({"connection":{"id":"personal","bucket":"photos","region":"us-east-005","prefix":"library","endpoint":"https://s3.us-east-005.backblazeb2.com"},"origin":ORIGIN})).unwrap()
}
fn request(action: Value) -> Value {
    json!({"connection":"personal","prefix":"library","bucketRoot":"https://s3.us-east-005.backblazeb2.com/photos/","action":action})
}
fn head() -> Head {
    Head::next(
        None,
        RecordRef { key: object_key("library", 1, "changes", &"a".repeat(64)), hash: lightcraft_catalog::assets::BlobId("a".repeat(64)), size: 10 },
        "example-library",
        None,
        crate::cloud_control::Control::initial("example-library"),
    )
    .unwrap()
}
fn raw(route: &str, origin: &str, token: &str, body: &str) -> String {
    format!(
        "POST {route} HTTP/1.1\r\nHost: localhost\r\nOrigin: {origin}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}
/// Exercise the real HTTP/parser/auth/scope boundary with a counted native seam.
/// Native B2 authority tests separately exercise real transition/CAS validation.
fn exchange(raw: &str, called: bool, failure: Option<CloudFailure>) -> (u16, Value) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let task = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut invoked = false;
        let config = config();
        config.validate().unwrap();
        handle_using(stream, &config, TOKEN, |request| {
            invoked = true;
            match failure {
                Some(failure) => Err(failure),
                None if matches!(request.action, Action::Read {}) => Ok(Reply::Read { current: None }),
                None => Ok(Reply::Updated {}),
            }
        });
        invoked
    });
    let mut stream = TcpStream::connect(address).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    stream.write_all(raw.as_bytes()).unwrap();
    stream.shutdown(Shutdown::Write).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let invoked = task.join().unwrap();
    assert_eq!(invoked, called, "native control must not run before all boundary checks pass: {}", raw.split("\r\n\r\n").nth(1).unwrap_or_default());
    assert!(!response.contains(TOKEN) && !response.contains("bucketInfo") && !response.contains("authorizationToken"));
    let (headers, body) = response.split_once("\r\n\r\n").unwrap();
    if called {
        assert!(headers.contains("Access-Control-Allow-Origin: http://127.0.0.1:8080"));
        assert!(headers.contains("Cache-Control: no-store"));
    }
    (headers.split_whitespace().nth(1).unwrap().parse().unwrap(), if body.is_empty() { Value::Null } else { serde_json::from_str(body).unwrap() })
}
fn post(body: Value, called: bool) -> (u16, Value) {
    exchange(&raw("/authorize/control", ORIGIN, TOKEN, &body.to_string()), called, None)
}

#[test]
fn control_http_authorizes_only_typed_destination_bound_operations() {
    let actions = [
        json!({"operation":"read"}),
        json!({"operation":"initialize","head":head()}),
        json!({"operation":"write","expected":format!("b2:12:{}", "b".repeat(64)),"head":head()}),
        json!({"operation":"fence","expected":format!("b2:12:{}", "b".repeat(64)),"library":"example-library"}),
    ];
    for action in actions {
        let (status, body) = post(request(action.clone()), true);
        assert_eq!(status, 200);
        assert_eq!(body["result"], if action["operation"] == "read" { "read" } else { "updated" });
    }
    let valid = request(json!({"operation":"read"}));
    assert_eq!(exchange(&format!("OPTIONS /authorize/control HTTP/1.1\r\nHost: localhost\r\nOrigin: {ORIGIN}\r\nAccess-Control-Request-Method: POST\r\nAccess-Control-Request-Headers: authorization, content-type\r\n\r\n"), false, None).0, 204);
    assert_eq!(exchange("POST /authorize/control?x=1 HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n", false, None).0, 404);
    assert_eq!(exchange(&raw("/authorize/control", ORIGIN, "wrong", &valid.to_string()), false, None).0, 401);
    assert_eq!(exchange(&raw("/authorize/control", "https://other.example", TOKEN, &valid.to_string()), false, None).0, 403);
    for (field, value) in [("connection", "another"), ("prefix", "library/other"), ("bucketRoot", "https://s3.us-east-005.backblazeb2.com/other/")] {
        let mut bad = valid.clone();
        bad[field] = json!(value);
        assert_eq!(post(bad, false).0, 403, "{field}");
    }
    for root in [
        "https://s3.us-west-004.backblazeb2.com/photos/",
        "https://s3.us-east-005.backblazeb2.com/photos/?",
        "https://s3.us-east-005.backblazeb2.com/photos/#",
        "https://s3.us-east-005.backblazeb2.com/photos/../photos/",
        "https://name@s3.us-east-005.backblazeb2.com/photos/",
    ] {
        let mut bad = valid.clone();
        bad["bucketRoot"] = json!(root);
        assert_eq!(post(bad, false).0, 403, "{root}");
    }
    for action in [json!({"operation":"read","unexpected":true}), json!({"operation":"erase"}), json!({"operation":"read","bucketInfo":{}})] {
        assert_eq!(post(request(action), false).0, 400);
    }
    let mut bad = valid;
    bad["endpoint"] = json!("https://other.example");
    assert_eq!(post(bad, false).0, 400);
}

#[test]
fn control_http_bounds_payload_and_preserves_typed_retry_and_recovery() {
    for expected in ["", "b2:01:a", "b2:12:arbitrary", "b2:12:\n"] {
        assert_eq!(post(request(json!({"operation":"write","expected":expected,"head":head()})), false).0, 422);
    }
    let mut future = serde_json::to_value(head()).unwrap();
    future["version"] = json!(crate::cloud_records::HEAD_VERSION + 1);
    let (status, body) = post(request(json!({"operation":"initialize","head":future})), false);
    assert_eq!(status, 422);
    assert_eq!(body["failure"]["remedy"], "update");
    let mut oversized = head();
    oversized.library = Some("x".repeat(17 << 10));
    assert_eq!(post(request(json!({"operation":"initialize","head":oversized})), false).0, 422);
    for (route, size) in [("/authorize", 16385), ("/authorize/control", b2_control::REQUEST_LIMIT + 1)] {
        assert_eq!(exchange(&format!("POST {route} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {size}\r\n\r\n"), false, None).0, 413);
    }
    let mut failure = CloudFailure::http("B2 control", 503)
        .with_remedy(Recovery::FinishUpgrade)
        .with_recovery("Finish the same upgrade; pending edits are retained.");
    failure.retry_after_ms = Some(7000);
    let (status, body) = exchange(&raw("/authorize/control", ORIGIN, TOKEN, &request(json!({"operation":"read"})).to_string()), true, Some(failure));
    assert_eq!(status, 503);
    assert_eq!(body["failure"]["kind"], "connectivity");
    assert_eq!(body["failure"]["httpStatus"], 503);
    assert_eq!(body["failure"]["retryAfterMs"], 7000);
    assert_eq!(body["failure"]["remedy"], "finishUpgrade");
    let failure = CloudFailure::new(FailureKind::Contention, "authority changed; reread");
    let (status, body) = exchange(&raw("/authorize/control", ORIGIN, TOKEN, &request(json!({"operation":"read"})).to_string()), true, Some(failure));
    assert_eq!(status, 409);
    assert_eq!(body["failure"]["kind"], "contention");
}

#[test]
fn control_validation_refuses_custom_hosts_and_terminal_deletion_without_network() {
    let mut c = config().connection;
    c.endpoint = Some("https://us1".into());
    c.region = "us1".into();
    let body: Request =
        serde_json::from_value(json!({"connection":"personal","prefix":"library","bucketRoot":"https://us1/photos/","action":{"operation":"read"}}))
            .unwrap();
    assert_eq!(validate(&c, &body).unwrap_err().kind, FailureKind::Unsupported);
    let mut deleted = head();
    deleted.control = Some(deleted.control().unwrap().delete().unwrap());
    let body: Request =
        serde_json::from_value(request(json!({"operation":"write","expected":format!("b2:12:{}", "b".repeat(64)),"head":deleted}))).unwrap();
    assert!(validate(&config().connection, &body).is_err());
    assert_eq!(status(&CloudFailure::new(FailureKind::Authentication, "missing writeBuckets")), 403);
}
