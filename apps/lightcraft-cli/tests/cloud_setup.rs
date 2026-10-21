//! Real-binary connection checks for cloud setup against a loopback S3-shaped server.
#[allow(dead_code)]
#[path = "support/cloud_upload.rs"]
mod support;
use serde_json::{Value, json};
use support::Fixture;

fn config(f: &Fixture, profile: &str) -> Value {
    json!({"id":"personal","bucket":"photos","region":"us-east-1","prefix":"library","endpoint":f.endpoint,"profile":profile})
}

fn test_connection(f: &Fixture, profile: &str) -> Value {
    let (_, rows) = f.run(&f.dir.join("probe"), &[("cloud.testConnection", config(f, profile))]);
    rows.into_iter().next().unwrap()
}

#[test]
fn the_check_finds_append_and_head_libraries_and_never_writes() {
    let f = Fixture::new();
    f.state.lock().unwrap().append_mode = true;
    let empty = test_connection(&f, "acceptance");
    assert_eq!(empty["result"], json!({"libraries": [], "headTaken": false}), "{empty}");
    let a = f.dir.join("a");
    let (out, rows) = f.run(
        &a,
        &[
            ("cloud.configure", config(&f, "acceptance")),
            ("cloud.syncEnable", json!({"connection":"personal","protocol":"append-log-v1"})),
            ("cloud.sync", json!({})),
        ],
    );
    assert!(out.status.success(), "{rows:?} {}", String::from_utf8_lossy(&out.stderr));
    let library = rows[1]["result"]["library"].as_str().unwrap().to_owned();
    let writes = f.state.lock().unwrap().requests.iter().filter(|(method, _)| method != "GET").count();
    let found = test_connection(&f, "acceptance");
    assert_eq!(found["result"]["libraries"], json!([{"id": library, "protocol": "append-log-v1"}]), "{found}");
    assert_eq!(found["result"]["headTaken"], false);
    // a conditional head in the same folder is found too (one per folder)
    let head = json!({"version":3,"generation":1,"library":"b".repeat(64),"tip":{"key":format!("library/catalog/changes/{}.json", "a".repeat(64)),"hash":"a".repeat(64),"size":1}});
    f.state.lock().unwrap().objects.insert("/photos/library/catalog/head.json".into(), serde_json::to_vec(&head).unwrap());
    let both = test_connection(&f, "acceptance");
    assert_eq!(both["result"]["headTaken"], true, "{both}");
    assert_eq!(both["result"]["libraries"][0], json!({"id": "b".repeat(64), "protocol": "conditional-head-v1"}));
    let state = f.state.lock().unwrap();
    assert_eq!(state.requests.iter().filter(|(method, _)| method != "GET").count(), writes, "the check only reads");
}

#[test]
fn b2_download_cap_is_not_reported_as_rejected_keys_even_for_an_empty_bucket() {
    let f = Fixture::new();
    f.state.lock().unwrap().download_cap = true;
    let refused = test_connection(&f, "acceptance");
    let message = refused["error"].as_str().unwrap();
    assert!(message.contains("Backblaze B2 usage limit reached"), "{refused}");
    assert!(message.contains("Caps & Alerts"), "{refused}");
    assert!(!message.contains("refused these keys"));
    let state = f.state.lock().unwrap();
    assert!(state.objects.is_empty());
    assert!(state.requests.iter().all(|(method, _)| method == "GET"));
    assert_eq!(state.signature_errors, 0);
}

#[test]
fn b2_download_cap_preserves_original_download_provenance_without_writing_bytes() {
    use lightcraft_engine::assets::{
        AssetError, BlobStore,
        s3::{AccessKeys, S3Connection},
    };
    let f = Fixture::new();
    {
        let mut state = f.state.lock().unwrap();
        state.download_cap = true;
        state.allow_typed_keys = true;
    }
    let connection: S3Connection = serde_json::from_value(config(&f, "unused")).unwrap();
    let connection = connection.with_keys(AccessKeys::new("EXAMPLE_ACCESS", "EXAMPLE_SECRET").unwrap());
    let blob = serde_json::from_value(json!({
        "id":support::hash(b"photo"),"size":5,
        "remote":{"connection":"personal","key":"library/photo.jpg"}
    }))
    .unwrap();
    let mut output = Vec::new();
    let error = connection.download(&blob, &mut output, &std::sync::atomic::AtomicBool::new(false), &mut |_| {}).unwrap_err();
    assert_eq!(error, AssetError::ProviderLimit { status: 403 });
    assert!(output.is_empty());
    assert_eq!(f.state.lock().unwrap().signature_errors, 0);
}

#[test]
fn the_check_says_what_to_fix() {
    let f = Fixture::new();
    let credentials = f.dir.join("credentials");
    let mut text = std::fs::read_to_string(&credentials).unwrap();
    text.push_str("[wrong]\naws_access_key_id=EXAMPLE_ACCESS\naws_secret_access_key=WRONG\naws_session_token=EXAMPLE_SESSION\n");
    std::fs::write(&credentials, text).unwrap();
    let refused = test_connection(&f, "wrong");
    assert!(refused["error"].as_str().unwrap().contains("refused these keys"), "{refused}");
    let missing = test_connection(&f, "absent");
    assert!(missing["error"].as_str().unwrap().contains("No keys to connect with"), "{missing}");
    let mut bad = config(&f, "acceptance");
    bad["bucket"] = json!("Not A Bucket");
    let (_, rows) = f.run(&f.dir.join("probe"), &[("cloud.testConnection", bad)]);
    assert!(rows[0]["error"].as_str().unwrap().contains("invalid connection"), "{rows:?}");
}
