//! Request accounting acceptance uses only signed loopback storage, never live B2.
#[allow(dead_code)]
#[path = "support/cloud_upload.rs"]
mod support;
use serde_json::{Value, json};
use support::Fixture;

fn config(f: &Fixture) -> Value {
    json!({"id":"personal","bucket":"photos","region":"us-east-1","prefix":"library","endpoint":f.endpoint,"profile":"acceptance"})
}

#[test]
fn sync_and_import_totals_include_media_verification_and_match_the_counting_backend() {
    for protocol in ["append-log-v1", "conditional-head-v1"] {
        let f = Fixture::new();
        f.state.lock().unwrap().append_mode = protocol == "append-log-v1";
        let pixels = lightcraft_raster::Rgba8::from_fn(32, 24, |x, y| [x as u8 * 5, y as u8 * 7, 127, 255]);
        let image = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&pixels), &Default::default()).unwrap();
        let path = f.dir.join("source.png");
        std::fs::write(&path, image).unwrap();
        let (out, rows) = f.run(
            &f.dir.join("library"),
            &[
                ("cloud.configure", config(&f)),
                ("cloud.syncEnable", json!({"connection":"personal","protocol":protocol})),
                ("cloud.import", json!({"connection":"personal","paths":[path]})),
                ("cloud.upload", json!({})),
                ("cloud.sync", json!({"audit":true})),
                ("cloud.traffic", json!({"limit":256})),
                ("cloud.overview", json!({})),
                ("cloud.traffic", json!({"limit":256})),
            ],
        );
        assert!(out.status.success(), "{rows:?} {}", String::from_utf8_lossy(&out.stderr));
        let report = &rows[5]["result"];
        let final_report = &rows[7]["result"];
        if let Some(root) = std::env::var_os("LIGHTCRAFT_CLOUD_TRAFFIC_ARTIFACTS") {
            let root = std::path::PathBuf::from(root);
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(root.join(format!("{protocol}.json")), serde_json::to_vec_pretty(report).unwrap()).unwrap();
        }
        assert_eq!(report["scope"], "thisProcess");
        assert_eq!(report["totals"], final_report["totals"], "querying traffic causes no extra storage work");
        assert_eq!(report["totals"], rows[6]["result"]["traffic"]["totals"]);
        let state = f.state.lock().unwrap();
        assert_eq!(report["totals"]["attempts"], state.requests.len() as u64, "{report}");
        assert_eq!(report["totals"]["responses"], state.requests.len() as u64);
        assert_eq!(report["totals"]["bodyBytesSent"], state.traffic.iter().map(|t| t.request_bytes).sum::<u64>());
        assert_eq!(
            report["totals"]["bodyBytesRead"],
            state.traffic.iter().map(|t| t.response_bytes).sum::<u64>(),
            "all fixture bodies in this successful flow are consumed"
        );
        let requests = report["recentRequests"].as_array().unwrap();
        assert_eq!(requests.len(), state.requests.len(), "fixture fits the bounded recent-request window");
        let media = requests.iter().filter(|r| r["transport"] == "s3Media" && r["bodyBytesRead"].as_u64().unwrap() > 0).collect::<Vec<_>>();
        assert!(!media.is_empty(), "full verification downloads must not bypass accounting");
        assert!(media.iter().all(|r| r["purpose"] == "uploadVerification"));
        assert!(requests.iter().any(|r| r["purpose"] == "publication"));
        let operations = report["operations"].as_array().unwrap();
        assert!(operations.iter().any(|o| o["purpose"] == "sync" && o["totals"]["attempts"].as_u64().unwrap() > 0));
        assert!(operations.iter().any(|o| o["import"].is_string() && o["totals"]["bodyBytesSent"].as_u64().unwrap() > 0));
        assert_eq!(report["connectionBindings"][0]["connection"], "personal");
        for secret in ["EXAMPLE_ACCESS", "EXAMPLE_SECRET", "EXAMPLE_SESSION", f.endpoint.as_str(), path.to_str().unwrap()] {
            assert!(!report.to_string().contains(secret), "{secret}");
        }
        assert_eq!(state.signature_errors, 0);
    }
}

#[test]
fn cap_errors_are_counted_with_their_response_bytes_and_typed_refusal() {
    let f = Fixture::new();
    f.state.lock().unwrap().download_cap = true;
    let (out, rows) = f.run(&f.dir.join("empty"), &[("cloud.testConnection", config(&f)), ("cloud.traffic", json!({"limit":256}))]);
    assert!(!out.status.success());
    assert!(rows[0]["error"].as_str().unwrap().contains("B2 download limit"));
    let report = &rows[1]["result"];
    let state = f.state.lock().unwrap();
    assert_eq!(report["totals"]["attempts"], state.requests.len() as u64);
    assert_eq!(report["totals"]["bodyBytesRead"], state.traffic.iter().map(|t| t.response_bytes).sum::<u64>());
    assert_eq!(report["totals"]["providerCaps"], 1);
    assert!(
        report["recentRequests"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["status"] == 403 && r["providerRefusal"] == "b2DownloadOrClassBCapExceeded" && r["purpose"] == "setup")
    );
    assert!(state.requests.iter().all(|(method, _)| method == "GET"));
    assert_eq!(state.signature_errors, 0);
}

#[test]
fn traffic_queries_reject_hostile_parameters_without_requests_or_a_crash() {
    let f = Fixture::new();
    let (out, rows) = f.run(
        &f.dir.join("queries"),
        &[
            ("cloud.configure", config(&f)),
            ("cloud.traffic", json!({"limit":257})),
            ("cloud.traffic", json!({"limit":-1})),
            ("cloud.traffic", json!({"limit":"lots"})),
            ("cloud.traffic", json!({"operation":u64::MAX})),
            ("cloud.traffic", json!({"limit":0})),
        ],
    );
    assert!(!out.status.success());
    assert_eq!(rows.len(), 6);
    for row in &rows[1..5] {
        assert_eq!(row["ok"], false, "{row}");
        assert!(row["error"].as_str().unwrap().contains("invalid parameters"));
    }
    assert_eq!(rows[5]["ok"], true);
    assert_eq!(rows[5]["result"]["totals"]["attempts"], 0);
    assert!(rows[5]["result"]["recentRequests"].as_array().unwrap().is_empty());
    assert!(f.state.lock().unwrap().requests.is_empty());
}
