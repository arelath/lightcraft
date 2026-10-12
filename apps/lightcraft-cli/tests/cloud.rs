//! Real-binary cloud-only RAW acceptance using a loopback S3-shaped server and fake credentials.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

const BIN: &str = env!("CARGO_BIN_EXE_lightcraft-cli");
static NEXT: AtomicUsize = AtomicUsize::new(0);

fn raw_bytes() -> Vec<u8> {
    use lightcraft_raw::*;
    let (w, h) = (32, 24);
    let raw = RawImage {
        format: RawFormat::Dng,
        width: w,
        height: h,
        cpp: 1,
        data: RawData::U16((0..w * h).map(|i| 256 + ((i % w) * 300 + (i / w) * 200) as u16).collect()),
        cfa: Some(Cfa::bayer("RGGB").unwrap()),
        bits: 14,
        black: BlackLevel::uniform(256.0),
        white: vec![16000.0],
        active_area: Rect::new(0, 0, w, h),
        crop: Rect::new(0, 0, w, h),
        orientation: Orientation::Normal,
        color: ColorData {
            illuminant: [17, 21],
            color_matrix: [
                Some(lightcraft_color::Mat3([[0.9, 0.2, -0.15], [-0.3, 1.25, 0.08], [0.02, -0.12, 0.85]])),
                Some(lightcraft_color::Mat3([[0.7, 0.3, -0.1], [-0.35, 1.3, 0.1], [0.05, -0.2, 1.0]])),
            ],
            as_shot_neutral: Some([0.5, 1.0, 0.7]),
            ..Default::default()
        },
        wb_multipliers: None,
        linearized: false,
        opcodes: Default::default(),
        metadata: Default::default(),
    };
    write_dng(&raw, &DngWriteOptions::default()).unwrap()
}

struct Fixture {
    dir: PathBuf,
    endpoint: String,
    requests: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
    bytes: Vec<u8>,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("lightcraft-cloud-binary-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("credentials"),
            "[acceptance]\naws_access_key_id=EXAMPLE_ACCESS\naws_secret_access_key=EXAMPLE_SECRET\naws_session_token=EXAMPLE_SESSION\n",
        )
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(AtomicUsize::new(0));
        let bytes = raw_bytes();
        let (thread_stop, thread_requests, payload) = (stop.clone(), requests.clone(), bytes.clone());
        let worker = std::thread::spawn(move || {
            while !thread_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
                        let mut reader = BufReader::new(stream);
                        let mut request = String::new();
                        loop {
                            let mut line = String::new();
                            if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                                break;
                            }
                            request.push_str(&line);
                            assert!(request.len() < 32 << 10);
                        }
                        assert!(request.starts_with("GET /photos/library/cloud.dng HTTP/1.1\r\n"));
                        assert!(request.contains("Authorization: AWS4-HMAC-SHA256 Credential=EXAMPLE_ACCESS/"));
                        assert!(request.contains("x-amz-security-token: EXAMPLE_SESSION"));
                        thread_requests.fetch_add(1, Ordering::Relaxed);
                        let mut stream = reader.into_inner();
                        write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: \"not-a-checksum\"\r\nConnection: close\r\n\r\n",
                            payload.len()
                        )
                        .unwrap();
                        stream.write_all(&payload).unwrap();
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(std::time::Duration::from_millis(10)),
                    Err(e) => panic!("mock S3: {e}"),
                }
            }
        });
        Self { dir, endpoint, requests, stop, worker: Some(worker), bytes }
    }
    fn command(&self) -> Command {
        let mut c = Command::new(BIN);
        c.env("AWS_SHARED_CREDENTIALS_FILE", self.dir.join("credentials"));
        c.env("LIGHTCRAFT_CONFIG_DIR", self.dir.join("config"));
        c
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn one_shot_cli_waits_for_uncached_pins_and_retries_a_removed_original() {
    let fixture = Fixture::new();
    let library = fixture.dir.join("library");
    let config = json!({"id": "personal", "bucket": "photos", "region": "us-east-1", "prefix": "library", "endpoint": fixture.endpoint, "profile": "acceptance"});
    let attach = json!({"assetId": "waited-raw", "asset": {"original": {"id": format!("{:x}", Sha256::digest(&fixture.bytes)), "size": fixture.bytes.len(),
        "remote": {"connection": "personal", "key": "library/cloud.dng"}}}, "fileName": "cloud.dng", "format": "DNG", "kind": "raw", "width": 32, "height": 24});
    let result = fixture
        .command()
        .args(["run", "--library"])
        .arg(&library)
        .arg("cloud.configure")
        .arg(config.to_string())
        .arg("cloud.attach")
        .arg(attach.to_string())
        .args(["cloud.pin", "quality=original", "wait=true", "cloud.status"])
        .output()
        .unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    let rows: Vec<Value> = String::from_utf8_lossy(&result.stdout).lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    assert!(rows.iter().all(|row| row["ok"] == true), "{rows:?}");
    assert_eq!(rows[2]["result"]["background"], false);
    assert_eq!(rows[3]["result"]["photos"][0]["original"]["state"], "cached");
    assert_eq!(fixture.requests.load(Ordering::Relaxed), 1);
    // Reopening starts recovery of persisted pins. Release the owner in its own
    // session before removal, so that a still-running recovery lease is not mistaken
    // for a failure of the original-removal contract.
    let unpinned = fixture.command().args(["run", "--library"]).arg(&library).args(["cloud.unpin", "ids=[1]"]).output().unwrap();
    assert!(unpinned.status.success(), "{}", String::from_utf8_lossy(&unpinned.stderr));
    let result = fixture
        .command()
        .args(["run", "--library"])
        .arg(&library)
        .args(["cloud.removeLocalCopy", "ids=[1]", "cloud.fetch", "ids=[1]", "wait=true", "cloud.status", "ids=[1]"])
        .output()
        .unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    let rows: Vec<Value> = String::from_utf8_lossy(&result.stdout).lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    assert!(rows.iter().all(|row| row["ok"] == true), "{rows:?}");
    assert_eq!(rows[0]["result"]["removed"].as_array().unwrap().len(), 1, "{rows:?}");
    assert_eq!(rows[1]["result"]["completed"], 1);
    assert_eq!(rows[2]["result"]["photos"][0]["original"]["state"], "cached");
    assert_eq!(fixture.requests.load(Ordering::Relaxed), 2);
}

#[test]
fn s3_only_raw_is_edited_exported_and_reopened_through_cli_and_mcp() {
    let fixture = Fixture::new();
    if let Some(path) = std::env::var_os("LIGHTCRAFT_CLOUD_ACCEPTANCE_DIR") {
        let dir = PathBuf::from(path);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("cloud.dng"), &fixture.bytes).unwrap();
    }
    let library = fixture.dir.join("library");
    let out = fixture.dir.join("cli.png");
    let config = json!({"id": "personal", "bucket": "photos", "region": "us-east-1", "prefix": "library", "endpoint": fixture.endpoint, "profile": "acceptance"});
    let attach = json!({"assetId": "shared-raw", "asset": {"original": {"id": format!("{:x}", Sha256::digest(&fixture.bytes)), "size": fixture.bytes.len(),
        "remote": {"connection": "personal", "key": "library/cloud.dng"}}}, "fileName": "cloud.dng", "format": "DNG", "kind": "raw", "width": 32, "height": 24});
    let result = fixture
        .command()
        .args(["run", "--library"])
        .arg(&library)
        .arg("cloud.configure")
        .arg(config.to_string())
        .arg("cloud.attach")
        .arg(attach.to_string())
        .args(["develop.set", "control=light.exposure", "value=0.75", "app.export"])
        .arg(format!("path={}", out.display()))
        .args(["format=png", "longEdge=0"])
        .args(["cloud.pin", "quality=original", "cloud.cache", "budgetMb=0"])
        .output()
        .unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    let results: Vec<Value> = String::from_utf8_lossy(&result.stdout).lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    assert_eq!(results.len(), 6, "{results:?}");
    assert!(results.iter().all(|r| r["ok"] == true), "{results:?}");
    assert_eq!(results[3]["result"]["width"], 32);
    assert_eq!(results[5]["result"]["budgetBytes"], 0);
    assert_eq!(results[5]["result"]["pins"]["photo:1"]["quality"], "original");
    assert!(out.is_file());
    assert_eq!(fixture.requests.load(Ordering::Relaxed), 1);

    let mut process = fixture
        .command()
        .args(["mcp", "--library"])
        .arg(&library)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = process.stdin.take().unwrap();
    let mut output = BufReader::new(process.stdout.take().unwrap());
    let mut rpc = |request: Value| {
        writeln!(input, "{request}").unwrap();
        input.flush().unwrap();
        let mut line = String::new();
        assert!(output.read_line(&mut line).unwrap() > 0);
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert!(reply.get("error").is_none(), "{reply}");
        reply
    };
    rpc(
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "cloud-test", "version": "1"}}}),
    );
    let edit = rpc(
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "run_command", "arguments": {"command": "develop.get", "params": {"id": 1}}}}),
    );
    assert_eq!(edit["result"]["isError"], false, "{edit}");
    assert_eq!(edit["result"]["structuredContent"]["light"]["exposure"], 0.75);
    let out = fixture.dir.join("mcp.png");
    let exported = rpc(
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "export", "arguments": {"ids": [1], "path": out, "format": "png", "longEdge": 0}}}),
    );
    assert_eq!(exported["result"]["isError"], false, "{exported}");
    assert!(out.is_file());
    let retained = rpc(
        json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "run_command", "arguments": {"command": "cloud.cache", "params": {}}}}),
    );
    assert_eq!(retained["result"]["structuredContent"]["pins"]["photo:1"]["quality"], "original");
    let protected = rpc(
        json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {"name": "run_command", "arguments": {"command": "cloud.removeLocalCopy", "params": {"ids": [1]}}}}),
    );
    assert_eq!(protected["result"]["structuredContent"]["removed"], json!([]));
    assert_eq!(protected["result"]["structuredContent"]["failed"].as_array().unwrap().len(), 1);
    drop(input);
    assert!(process.wait().unwrap().success());
    assert_eq!(fixture.requests.load(Ordering::Relaxed), 1, "reopening must use verified cache");
}
