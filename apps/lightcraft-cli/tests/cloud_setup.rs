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
