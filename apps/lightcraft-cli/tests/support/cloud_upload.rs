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

// Shared fixture: only transfer-cost suites inspect these counters.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct Traffic {
    pub method: String,
    pub target: String,
    pub request_bytes: u64,
    pub response_bytes: u64,
    pub status: u16,
}
#[derive(Default)]
pub struct State {
    pub append_mode: bool,
    pub list_page_size: usize,
    pub list_calls: usize,
    pub fail_list_after: Option<usize>,
    pub late_object: Option<(String, Vec<u8>)>,
    pub objects: BTreeMap<String, Vec<u8>>,
    pub multipart: BTreeMap<String, (String, BTreeMap<u16, Vec<u8>>)>,
    pub historical_versions: BTreeMap<(String, String), Option<Vec<u8>>>,
    pub track_versions: bool,
    pub object_after_erasure_barrier: Option<(String, Vec<u8>)>,
    pub requests: Vec<(String, String)>,
    pub traffic: Vec<Traffic>,
    pub created: usize,
    pub lost_part: bool,
    pub lost_put: bool,
    pub lost_complete: bool,
    pub lost_head: bool,
    /// Lose the response of the first accepted head PUT whose body contains this text.
    pub lost_head_matching: Option<String>,
    /// Refuse matching object PUTs before mutation: (path fragment, body fragment).
    pub refuse_put: Option<(String, String)>,
    pub stop_after_recovery_prefix: bool,
    pub embedded_error: bool,
    pub unauthorized: bool,
    pub reject_status: Option<u16>,
    pub signature_errors: usize,
    pub hold_part: bool,
    pub hold_head: bool,
    pub hold_get: Option<String>,
    pub hold_abort: bool,
    pub defer_abort: usize,
    pub unsupported_conditional: bool,
    pub delete_conflict: bool,
    pub versioned_media: bool,
    pub retention_modified: Option<String>,
    pub next_head: Option<Vec<u8>>,
    /// Swap an isolated fixture snapshot after serving an exact GET response.
    pub replace_after_get: Option<(String, BTreeMap<String, Vec<u8>>)>,
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
                let request_bytes = body.len() as u64;
                let (status, response, etag, lose) = if !valid || state.unauthorized {
                    if !valid {
                        state.signature_errors += 1;
                    }
                    (403, Vec::new(), None, false)
                } else if let Some(status) = state.reject_status {
                    (status, Vec::new(), None, false)
                } else {
                    serve(&mut state, &method, &target, &headers, body)
                };
                if method == "GET"
                    && state.replace_after_get.as_ref().is_some_and(|(trigger, _)| trigger == &target)
                    && let Some((_, objects)) = state.replace_after_get.take()
                {
                    state.objects = objects;
                }
                let hold_part = state.hold_part && method == "PUT" && target.contains("partNumber=1&");
                let hold_head = state.hold_head && method == "PUT" && target.ends_with("/catalog/head.json");
                let hold_get = method == "GET" && state.hold_get.as_ref() == Some(&target);
                let hold_abort = state.hold_abort && method == "DELETE" && target.contains("uploadId=");
                state.traffic.push(Traffic {
                    method: method.clone(),
                    target: target.clone(),
                    request_bytes,
                    response_bytes: if method == "HEAD" || lose || hold_part || hold_head || hold_abort { 0 } else { response.len() as u64 },
                    status,
                });
                let versioned_media = state.versioned_media;
                let retention_modified = state.retention_modified.clone();
                drop(state);
                if hold_part || hold_head || hold_abort || hold_get {
                    while !worker_stop.load(Ordering::Relaxed) {
                        let state = worker_state.lock().unwrap();
                        let held = (hold_part && state.hold_part)
                            || (hold_head && state.hold_head)
                            || (hold_abort && state.hold_abort)
                            || (hold_get && state.hold_get.as_ref() == Some(&target));
                        drop(state);
                        if !held {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(2));
                    }
                    if !hold_get || worker_stop.load(Ordering::Relaxed) {
                        continue; // Accepted writes remain intentionally uncertain; held reads resume normally.
                    }
                }
                if lose {
                    continue;
                }
                let mut stream = reader.into_inner();
                write!(stream, "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n", response.len()).unwrap();
                if let Some(etag) = etag {
                    write!(stream, "ETag: {etag}\r\n").unwrap();
                }
                if method == "HEAD" {
                    write!(stream, "Last-Modified: {}\r\n", retention_modified.as_deref().unwrap_or("Mon, 01 Jan 2024 00:00:00 GMT")).unwrap();
                    if versioned_media {
                        stream.write_all(b"x-amz-version-id: fixture-version\r\n").unwrap();
                    }
                }
                stream.write_all(b"\r\n").unwrap();
                if method != "HEAD" {
                    stream.write_all(&response).unwrap();
                }
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
    #[allow(dead_code)]
    pub fn import(&self, library: &std::path::Path, large: bool) -> (String, PathBuf) {
        let result = self.import_unsynchronized(library, large);
        let (output, rows) = self.run(library, &[("cloud.syncEnable", json!({"connection":"personal"}))]);
        assert!(output.status.success(), "{rows:?} {}", String::from_utf8_lossy(&output.stderr));
        result
    }
    pub fn import_unsynchronized(&self, library: &std::path::Path, large: bool) -> (String, PathBuf) {
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
            &[
                ("cloud.configure", config),
                ("cloud.import", json!({"connection":"personal", "paths":[source],"duplicatePolicy":"allow"})),
                ("cloud.importStatus", json!({})),
            ],
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
    if method == "PUT" && state.refuse_put.as_ref().is_some_and(|(path, text)| target.contains(path) && String::from_utf8_lossy(&body).contains(text))
    {
        return (403, Vec::new(), None, false);
    }
    if state.unsupported_conditional && (headers.contains_key("if-none-match") || headers.contains_key("if-match")) {
        return (501, b"<Error><Code>NotImplemented</Code><Message>Conditional writes unavailable</Message></Error>".to_vec(), None, false);
    }
    let (key, query) = target.split_once('?').unwrap_or((target, ""));
    let query = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (decode(key), decode(value))
        })
        .collect::<BTreeMap<_, _>>();
    if method == "PUT"
        && serde_json::from_slice::<Value>(&body).ok().is_some_and(|v| v["state"] == "deleted" || v["control"]["state"] == "deleted")
        && let Some((key, bytes)) = state.object_after_erasure_barrier.take()
    {
        state.objects.insert(key, bytes);
    }
    if state.track_versions
        && method == "PUT"
        && query.is_empty()
        && let Some(old) = state.objects.get(key)
        && old != &body
    {
        let id = format!("v-{}", hash(old));
        state.historical_versions.insert((key.to_owned(), id), Some(old.clone()));
    }
    if method == "GET" && query.contains_key("versions") {
        let prefix = &query["prefix"];
        let escape = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;");
        let mut versions = BTreeMap::new();
        for (key, body) in &state.objects {
            if let Some(key) = key.strip_prefix("/photos/").filter(|k| k.starts_with(prefix)) {
                let id = if state.track_versions { format!("v-{}", hash(body)) } else { "null".into() };
                versions.insert((key.to_owned(), id), (Some(body.clone()), true));
            }
        }
        for ((key, id), body) in &state.historical_versions {
            if let Some(key) = key.strip_prefix("/photos/").filter(|k| k.starts_with(prefix)) {
                let full = format!("/photos/{key}");
                let latest = !state.objects.contains_key(&full)
                    && state
                        .historical_versions
                        .iter()
                        .filter(|((k, _), v)| k == &full && v.is_none())
                        .map(|((_, v), _)| v)
                        .next_back()
                        .is_some_and(|v| v == id);
                versions.insert((key.to_owned(), id.clone()), (body.clone(), latest));
            }
        }
        let marker = query.get("key-marker").zip(query.get("version-id-marker"));
        let entries: Vec<_> = versions.into_iter().filter(|((key, id), _)| marker.is_none_or(|(k, v)| (key, id) > (k, v))).collect();
        let max = if state.list_page_size == 0 { 1000 } else { state.list_page_size };
        let truncated = entries.len() > max;
        let page: Vec<_> = entries.into_iter().take(max).collect();
        let mut xml = format!("<ListVersionsResult><Prefix>{}</Prefix><IsTruncated>{truncated}</IsTruncated>", escape(prefix));
        for ((key, id), (body, latest)) in &page {
            let (kind, extra) = match body {
                Some(body) => ("Version", format!("<Size>{}</Size><ETag>{}</ETag>", body.len(), escape(&tag(body)))),
                None => ("DeleteMarker", String::new()),
            };
            xml.push_str(&format!(
                "<{kind}><Key>{}</Key><VersionId>{}</VersionId><IsLatest>{latest}</IsLatest>{extra}</{kind}>",
                escape(key),
                escape(id)
            ));
        }
        if truncated {
            let ((key, id), _) = page.last().unwrap();
            xml.push_str(&format!("<NextKeyMarker>{}</NextKeyMarker><NextVersionIdMarker>{}</NextVersionIdMarker>", escape(key), escape(id)));
        }
        xml.push_str("</ListVersionsResult>");
        return (200, xml.into_bytes(), None, false);
    }
    if method == "GET" && query.contains_key("uploads") {
        let prefix = &query["prefix"];
        let escape = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
        let mut xml = format!("<ListMultipartUploadsResult><Prefix>{}</Prefix><IsTruncated>false</IsTruncated>", escape(prefix));
        for (id, (key, _)) in &state.multipart {
            if let Some(key) = key.strip_prefix("/photos/").filter(|k| k.starts_with(prefix)) {
                xml.push_str(&format!("<Upload><Key>{}</Key><UploadId>{}</UploadId></Upload>", escape(key), escape(id)));
            }
        }
        xml.push_str("</ListMultipartUploadsResult>");
        return (200, xml.into_bytes(), None, false);
    }
    if method == "GET" && query.get("list-type").map(String::as_str) == Some("2") {
        assert_eq!(key, "/photos/");
        state.list_calls += 1;
        if state.fail_list_after == Some(state.list_calls) {
            return (503, Vec::new(), None, false);
        }
        let prefix = query.get("prefix").unwrap();
        assert!(prefix.starts_with("library/"));
        let delimiter = query.get("delimiter").map(String::as_str) == Some("/");
        if delimiter
            && query.contains_key("continuation-token")
            && let Some((key, body)) = state.late_object.take()
        {
            state.objects.insert(key, body);
        }
        let after = query.get("continuation-token").or_else(|| query.get("start-after"));
        let mut entries = std::collections::BTreeSet::new();
        for key in state.objects.keys().filter_map(|k| k.strip_prefix("/photos/")) {
            let Some(tail) = key.strip_prefix(prefix) else { continue };
            let item = if delimiter { tail.find('/').map_or_else(|| key.to_owned(), |n| format!("{prefix}{}/", &tail[..n])) } else { key.to_owned() };
            if after.is_none_or(|after| item > *after) {
                entries.insert(item);
            }
        }
        let max = query["max-keys"].parse::<usize>().unwrap().min(if state.list_page_size == 0 { 1000 } else { state.list_page_size });
        let truncated = entries.len() > max;
        let page: Vec<_> = entries.into_iter().take(max).collect();
        let escape = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
        let mut xml = format!("<ListBucketResult><Prefix>{}</Prefix><IsTruncated>{truncated}</IsTruncated>", escape(prefix));
        for item in &page {
            xml.push_str(&if delimiter && item.ends_with('/') {
                format!("<CommonPrefixes><Prefix>{}</Prefix></CommonPrefixes>", escape(item))
            } else {
                format!("<Contents><Key>{}</Key><Size>1</Size></Contents>", escape(item))
            });
        }
        if truncated {
            xml.push_str(&format!("<NextContinuationToken>{}</NextContinuationToken>", escape(page.last().unwrap())));
        }
        xml.push_str("</ListBucketResult>");
        return (200, xml.into_bytes(), None, false);
    }
    assert!(key.starts_with("/photos/library/"));
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
    } else if method == "GET" || method == "HEAD" {
        if let Some(bytes) = state.objects.get(key) {
            response = bytes.clone();
            etag = Some(tag(bytes));
        } else {
            status = 404;
        }
    } else if method == "DELETE" && query.contains_key("versionId") {
        let id = &query["versionId"];
        if state.delete_conflict {
            status = 412;
        } else if state.historical_versions.remove(&(key.to_owned(), id.clone())).is_some() {
            status = 204;
        } else if state
            .objects
            .get(key)
            .is_some_and(|body| if id == "null" { headers.get("if-match") == Some(&tag(body)) } else { id == &format!("v-{}", hash(body)) })
        {
            state.objects.remove(key);
            status = 204;
        } else {
            status = 404;
        }
    } else if method == "DELETE" && query.is_empty() {
        if state.delete_conflict || state.objects.get(key).is_none_or(|bytes| headers.get("if-match") != Some(&tag(bytes))) {
            status = 412;
        } else {
            state.objects.remove(key);
            status = 204;
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
                if !state.append_mode {
                    assert_eq!(headers.get("if-none-match").map(String::as_str), Some("*"));
                } else {
                    assert!(!headers.contains_key("if-none-match") && !headers.contains_key("if-match"));
                }
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
                    lose = std::mem::take(&mut state.lost_complete);
                }
            } else {
                status = 400;
            }
        } else {
            status = 404;
        }
    } else if method == "PUT" {
        if key.ends_with("/catalog/head.json")
            && let Some(competing) = state.next_head.take()
        {
            state.objects.insert(key.to_owned(), competing);
        }
        let absent = headers.get("if-none-match").map(String::as_str) == Some("*");
        let matches = headers.get("if-match");
        assert!(absent || matches.is_some() || (state.append_mode && !key.ends_with("/catalog/head.json")), "mutable writes must be conditional");
        if (absent && state.objects.contains_key(key)) || matches.is_some_and(|token| state.objects.get(key).is_none_or(|bytes| &tag(bytes) != token))
        {
            status = 412;
        } else {
            if key.ends_with("/catalog/head.json") {
                let head: Value = serde_json::from_slice(&body).unwrap();
                if head["control"]["state"] == "deleted" {
                    assert_eq!(head["tip"]["hash"], "0".repeat(64));
                    assert_eq!(head["generation"], 1);
                    assert!(head["checkpoint"].is_null());
                    assert!(head["control"]["migration"].is_null());
                    assert!(head["control"]["successor"].is_null());
                } else {
                    let batch_key = format!("/photos/{}", head["tip"]["key"].as_str().unwrap());
                    let batch: Value = serde_json::from_slice(&state.objects[&batch_key]).unwrap();
                    for imported in batch.get("imports").and_then(Value::as_array).into_iter().flatten() {
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
                    for mutation in batch.get("mutations").and_then(Value::as_array).into_iter().flatten() {
                        for change in mutation["changes"].as_array().unwrap().iter().filter(|change| change["field"] == "content") {
                            let asset = &change["value"]["asset"];
                            let mut blobs = vec![&asset["original"]];
                            blobs.extend([asset.get("smartPreview"), asset.get("thumbnail")].into_iter().flatten().filter(|blob| !blob.is_null()));
                            blobs.extend(asset.get("associated").and_then(Value::as_object).into_iter().flat_map(|map| map.values()));
                            for blob in blobs {
                                let object = format!("/photos/{}", blob["remote"]["key"].as_str().unwrap());
                                assert_eq!(
                                    hash(&state.objects[&object]),
                                    blob["id"].as_str().unwrap(),
                                    "mutation references only verified uploaded bytes"
                                );
                                assert_eq!(state.objects[&object].len() as u64, blob["size"].as_u64().unwrap());
                            }
                        }
                    }
                    if let Some(checkpoint) = head.get("checkpoint") {
                        let object = format!("/photos/{}", checkpoint["object"]["key"].as_str().unwrap());
                        assert_eq!(hash(&state.objects[&object]), checkpoint["object"]["hash"].as_str().unwrap());
                    }
                    if state.stop_after_recovery_prefix && head.get("checkpoint").is_some_and(|checkpoint| checkpoint["covers"] != batch["parent"]) {
                        state.stop_after_recovery_prefix = false;
                        state.unauthorized = true;
                    }
                }
                lose = std::mem::take(&mut state.lost_head);
                if state.lost_head_matching.as_ref().is_some_and(|text| String::from_utf8_lossy(&body).contains(text.as_str())) {
                    state.lost_head_matching = None;
                    lose = true;
                }
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
