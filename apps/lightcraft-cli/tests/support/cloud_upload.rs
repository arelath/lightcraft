//! Independent loopback S3 counterparty for real-binary upload and recovery tests.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

static NEXT: AtomicUsize = AtomicUsize::new(0);
pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn tag(bytes: &[u8]) -> String {
    format!("\"{}\"", &hash(bytes)[..32])
}
fn mac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let key = if key.len() > 64 { Sha256::digest(key).to_vec() } else { key.to_vec() };
    let (mut inner, mut outer) = ([0x36u8; 64], [0x5cu8; 64]);
    for (index, byte) in key.iter().enumerate() {
        inner[index] ^= byte;
        outer[index] ^= byte;
    }
    let mut h = Sha256::new();
    h.update(inner);
    h.update(data);
    let mut result = Sha256::new();
    result.update(outer);
    result.update(h.finalize());
    result.finalize().to_vec()
}
fn decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            out.push(u8::from_str_radix(&value[index + 1..index + 3], 16).unwrap());
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).unwrap()
}
fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) { char::from(byte).to_string() } else { format!("%{byte:02X}") })
        .collect()
}
fn signature(method: &str, target: &str, headers: &BTreeMap<String, String>, body: &[u8]) -> bool {
    let Some(auth) = headers.get("authorization").and_then(|value| value.strip_prefix("AWS4-HMAC-SHA256 ")) else { return false };
    let fields = auth.split(',').filter_map(|item| item.trim().split_once('=')).collect::<BTreeMap<_, _>>();
    let Some(credential) = fields.get("Credential") else { return false };
    let scope = credential.split('/').collect::<Vec<_>>();
    if scope.len() != 5 || scope[0] != "EXAMPLE_ACCESS" || scope[3] != "s3" || scope[4] != "aws4_request" {
        return false;
    }
    if headers.get("x-amz-content-sha256") != Some(&hash(body)) || headers.get("x-amz-security-token").map(String::as_str) != Some("EXAMPLE_SESSION")
    {
        return false;
    }
    let Some(signed) = fields.get("SignedHeaders") else { return false };
    let mut canonical_headers = String::new();
    for name in signed.split(';') {
        let Some(value) = headers.get(name) else { return false };
        canonical_headers.push_str(&format!("{name}:{}\n", value.split_whitespace().collect::<Vec<_>>().join(" ")));
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let mut pairs = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (encode(&decode(key)), encode(&decode(value)))
        })
        .collect::<Vec<_>>();
    pairs.sort();
    let query = pairs.iter().map(|(key, value)| format!("{key}={value}")).collect::<Vec<_>>().join("&");
    let canonical = format!("{method}\n{path}\n{query}\n{canonical_headers}\n{signed}\n{}", hash(body));
    let Some(date) = headers.get("x-amz-date") else { return false };
    let string = format!("AWS4-HMAC-SHA256\n{date}\n{}\n{}", scope[1..].join("/"), hash(canonical.as_bytes()));
    let mut key = b"AWS4EXAMPLE_SECRET".to_vec();
    for item in &scope[1..] {
        key = mac(&key, item.as_bytes());
    }
    let actual = mac(&key, string.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    fields.get("Signature") == Some(&actual.as_str())
}

#[derive(Default)]
pub struct State {
    pub objects: BTreeMap<String, Vec<u8>>,
    multipart: BTreeMap<String, (String, BTreeMap<u16, Vec<u8>>)>,
    pub requests: Vec<(String, String)>,
    pub created: usize,
    pub lost_part: bool,
    pub lost_put: bool,
    pub lost_head: bool,
    pub embedded_error: bool,
    pub unauthorized: bool,
    pub signature_errors: usize,
    pub hold_part: bool,
    pub hold_head: bool,
    pub hold_abort: bool,
    pub defer_abort: usize,
}

pub struct Fixture {
    pub dir: PathBuf,
    pub endpoint: String,
    pub state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Fixture {
    pub fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("lightcraft-upload-e2e-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("credentials"),
            "[acceptance]\naws_access_key_id=EXAMPLE_ACCESS\naws_secret_access_key=EXAMPLE_SECRET\naws_session_token=EXAMPLE_SESSION\n",
        )
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(State::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (worker_state, worker_stop) = (state.clone(), stop.clone());
        let worker = std::thread::spawn(move || {
            while !worker_stop.load(Ordering::Relaxed) {
                let stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(2));
                        continue;
                    }
                    Err(e) => panic!("fixture accept: {e}"),
                };
                stream.set_nonblocking(false).unwrap();
                stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
                stream.set_write_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let words = line.split_whitespace().collect::<Vec<_>>();
                let method = words[0].to_owned();
                let target = words[1].to_owned();
                let mut headers = BTreeMap::new();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    let (key, value) = line.split_once(':').unwrap();
                    headers.insert(key.to_lowercase(), value.trim().to_string());
                }
                let size = headers.get("content-length").map_or(0, |value| value.parse::<usize>().unwrap());
                assert!(size <= 16 << 20);
                let mut body = vec![0u8; size];
                reader.read_exact(&mut body).unwrap();
                let valid = signature(&method, &target, &headers, &body);
                let mut state = worker_state.lock().unwrap();
                state.requests.push((method.clone(), target.clone()));
                let (status, response, etag, lose) = if !valid || state.unauthorized {
                    if !valid {
                        state.signature_errors += 1;
                    }
                    (403, Vec::new(), None, false)
                } else {
                    serve(&mut state, &method, &target, &headers, body)
                };
                let hold_part = state.hold_part && method == "PUT" && target.contains("partNumber=1&");
                let hold_head = state.hold_head && method == "PUT" && target.ends_with("/catalog/head.json");
                let hold_abort = state.hold_abort && method == "DELETE" && target.contains("uploadId=");
                drop(state);
                if hold_part || hold_head || hold_abort {
                    while !worker_stop.load(Ordering::Relaxed) {
                        let state = worker_state.lock().unwrap();
                        let held = (hold_part && state.hold_part) || (hold_head && state.hold_head) || (hold_abort && state.hold_abort);
                        drop(state);
                        if !held {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(2));
                    }
                    continue; // Request was accepted; response remains intentionally uncertain.
                }
                if lose {
                    continue;
                }
                let mut stream = reader.into_inner();
                write!(stream, "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n", response.len()).unwrap();
                if let Some(etag) = etag {
                    write!(stream, "ETag: {etag}\r\n").unwrap();
                }
                stream.write_all(b"\r\n").unwrap();
                stream.write_all(&response).unwrap();
            }
        });
        Self { dir, endpoint, state, stop, worker: Some(worker) }
    }
    pub fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_lightcraft-cli"));
        command.env("AWS_SHARED_CREDENTIALS_FILE", self.dir.join("credentials"));
        command.env("LIGHTCRAFT_CONFIG_DIR", self.dir.join("config"));
        command
    }
    pub fn run(&self, library: &std::path::Path, commands: &[(&str, Value)]) -> (Output, Vec<Value>) {
        let mut command = self.command();
        command.args(["run", "--keep-going", "--library"]).arg(library);
        for (name, params) in commands {
            command.arg(name).arg(params.to_string());
        }
        let output = command.output().unwrap();
        let rows = String::from_utf8_lossy(&output.stdout).lines().map(|line| serde_json::from_str(line).unwrap()).collect();
        (output, rows)
    }
    pub fn import(&self, library: &std::path::Path, large: bool) -> (String, PathBuf) {
        let source = self.dir.join(format!("source-{}.png", NEXT.fetch_add(1, Ordering::Relaxed)));
        let pixels = lightcraft_raster::Rgba8::from_fn(32, 24, |x, y| [x as u8 * 5, y as u8 * 7, 100, 255]);
        let mut bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&pixels), &Default::default()).unwrap();
        if large {
            bytes.resize(9 << 20, 0);
        }
        std::fs::write(&source, bytes).unwrap();
        let config =
            json!({"id":"personal", "bucket":"photos", "region":"us-east-1", "prefix":"library", "endpoint":self.endpoint, "profile":"acceptance"});
        let (output, rows) = self.run(
            library,
            &[("cloud.configure", config), ("cloud.import", json!({"connection":"personal", "paths":[source]})), ("cloud.importStatus", json!({}))],
        );
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert!(rows.iter().all(|row| row["ok"] == true), "{rows:?}");
        let photo = &rows[1]["result"]["ids"][0];
        let id = rows[2]["result"]["uploads"].as_object().unwrap().iter().find(|(_, intent)| &intent["photoId"] == photo).unwrap().0.clone();
        (id, source)
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

fn serve(state: &mut State, method: &str, target: &str, headers: &BTreeMap<String, String>, body: Vec<u8>) -> (u16, Vec<u8>, Option<String>, bool) {
    let (key, query) = target.split_once('?').unwrap_or((target, ""));
    assert!(key.starts_with("/photos/library/"));
    let query = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (decode(key), decode(value))
        })
        .collect::<BTreeMap<_, _>>();
    let mut status = 200;
    let mut response = Vec::new();
    let mut etag = None;
    let mut lose = false;
    if method == "GET"
        && let Some(id) = query.get("uploadId")
    {
        if let Some((upload_key, parts)) = state.multipart.get(id) {
            assert_eq!(upload_key, key);
            let part = parts.keys().next().map(|number| format!("<Part><PartNumber>{number}</PartNumber></Part>")).unwrap_or_default();
            let object_key = key.strip_prefix("/photos/").unwrap();
            response = format!("<ListPartsResult><Bucket>photos</Bucket><Key>{object_key}</Key><UploadId>{id}</UploadId><IsTruncated>{}</IsTruncated>{part}</ListPartsResult>", parts.len() > 1).into_bytes();
        } else {
            status = 404;
        }
    } else if method == "GET" {
        if let Some(bytes) = state.objects.get(key) {
            response = bytes.clone();
            etag = Some(tag(bytes));
        } else {
            status = 404;
        }
    } else if method == "POST" && query.contains_key("uploads") {
        state.created += 1;
        let id = format!("fixture+/{}/=", state.created);
        state.multipart.insert(id.clone(), (key.to_string(), BTreeMap::new()));
        response = format!("<InitiateMultipartUploadResult><UploadId>{id}</UploadId></InitiateMultipartUploadResult>").into_bytes();
    } else if let Some(id) = query.get("uploadId") {
        if method == "DELETE" {
            status = if state.defer_abort > 0 {
                state.defer_abort -= 1;
                204
            } else if state.multipart.remove(id).is_some() {
                204
            } else {
                404
            };
        } else if let Some((upload_key, parts)) = state.multipart.get_mut(id) {
            assert_eq!(upload_key, key);
            if method == "PUT" {
                let number = query["partNumber"].parse().unwrap();
                etag = Some(tag(&body));
                parts.insert(number, body);
                lose = std::mem::take(&mut state.lost_part);
            } else if method == "POST" {
                assert_eq!(headers.get("if-none-match").map(String::as_str), Some("*"));
                if std::mem::take(&mut state.embedded_error) {
                    response = b"<Error><Code>InternalError</Code></Error>".to_vec();
                } else if state.objects.contains_key(key) {
                    status = 412;
                } else {
                    let xml = String::from_utf8(body).unwrap();
                    for (number, bytes) in parts.iter() {
                        assert!(xml.contains(&format!("<PartNumber>{number}</PartNumber>")));
                        assert!(xml.contains(&format!("<ETag>&quot;{}&quot;</ETag>", &hash(bytes)[..32])));
                    }
                    let bytes = parts.values().flatten().copied().collect::<Vec<_>>();
                    etag = Some(tag(&bytes));
                    response =
                        format!("<CompleteMultipartUploadResult><ETag>&quot;{}&quot;</ETag></CompleteMultipartUploadResult>", &hash(&bytes)[..32])
                            .into_bytes();
                    state.objects.insert(key.to_owned(), bytes);
                    state.multipart.remove(id);
                }
            } else {
                status = 400;
            }
        } else {
            status = 404;
        }
    } else if method == "PUT" {
        let absent = headers.get("if-none-match").map(String::as_str) == Some("*");
        let matches = headers.get("if-match");
        assert!(absent || matches.is_some(), "every write must be conditional");
        if (absent && state.objects.contains_key(key)) || matches.is_some_and(|token| state.objects.get(key).is_none_or(|bytes| &tag(bytes) != token))
        {
            status = 412;
        } else {
            if key.ends_with("/catalog/head.json") {
                let head: Value = serde_json::from_slice(&body).unwrap();
                let batch_key = format!("/photos/{}", head["tip"]["key"].as_str().unwrap());
                let batch: Value = serde_json::from_slice(&state.objects[&batch_key]).unwrap();
                for imported in batch["imports"].as_array().unwrap() {
                    for field in ["original", "smartPreview", "thumbnail"] {
                        let blob = &imported["asset"][field];
                        let object = format!("/photos/{}", blob["remote"]["key"].as_str().unwrap());
                        assert_eq!(
                            hash(&state.objects[&object]),
                            blob["id"].as_str().unwrap(),
                            "no catalog head may reference absent or wrong bytes"
                        );
                    }
                }
                lose = std::mem::take(&mut state.lost_head);
            } else {
                lose = std::mem::take(&mut state.lost_put);
            }
            etag = Some(tag(&body));
            state.objects.insert(key.to_string(), body);
        }
    } else {
        status = 400;
    }
    (status, response, etag, lose)
}
