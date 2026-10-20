//! Personal browser signer HTTP boundary. Deploy behind an HTTPS reverse proxy.
use crate::assets::s3::{BrowserRequest, S3Connection};
use hmac::{Hmac, Mac};
use lightcraft_fetch::http::Url;
use serde::Deserialize;
use sha2::Sha256;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    pub connection: S3Connection,
    pub origin: String,
    #[serde(default = "default_lifetime")]
    pub expires_seconds: u64,
}
fn default_lifetime() -> u64 {
    300
}
impl Config {
    pub fn validate(&self) -> Result<(), String> {
        self.connection.validate().map_err(|e| e.to_string())?;
        let url = Url::parse(&self.origin).map_err(|_| "invalid allowed browser origin")?;
        let host = if url.host.contains(':') { format!("[{}]", url.host.to_ascii_lowercase()) } else { url.host.to_ascii_lowercase() };
        let port = if url.port == if url.tls { 443 } else { 80 } { String::new() } else { format!(":{}", url.port) };
        let canonical = format!("{}://{host}{port}", if url.tls { "https" } else { "http" });
        if self.origin.chars().any(char::is_control)
            || self.origin.len() > 2048
            || self.origin.contains(['?', '#'])
            || url.path != "/"
            || self.origin.ends_with('/')
            || self.origin != canonical
            || (!url.tls && !matches!(url.host.as_str(), "127.0.0.1" | "localhost" | "::1"))
        {
            return Err("origin must be one exact HTTPS browser origin without a path (loopback HTTP is allowed for development)".into());
        }
        if self.connection.prefix.is_empty()
            || self.connection.prefix.ends_with('/')
            || self.connection.prefix.len() > 1024
            || !(30..=900).contains(&self.expires_seconds)
        {
            return Err("signer requires a dedicated nonempty prefix without trailing / and expiresSeconds between 30 and 900".into());
        }
        Ok(())
    }
}
mod control;
#[cfg(test)]
pub(crate) use control::execute as execute_b2_control;

struct Request {
    control: bool,
    method: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}
fn read_deadline(stream: &mut TcpStream, mut bytes: &mut [u8], deadline: Instant) -> Result<(), u16> {
    while !bytes.is_empty() {
        let remaining = deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()).ok_or(408u16)?;
        stream.set_read_timeout(Some(remaining)).map_err(|_| 500u16)?;
        let n = stream.read(bytes).map_err(|_| 408u16)?;
        if n == 0 {
            return Err(400);
        }
        bytes = bytes.get_mut(n..).ok_or(400u16)?;
    }
    Ok(())
}
fn read_request(stream: &mut TcpStream) -> Result<Request, u16> {
    stream.set_read_timeout(Some(Duration::from_secs(10))).map_err(|_| 500u16)?;
    stream.set_write_timeout(Some(Duration::from_secs(10))).map_err(|_| 500u16)?;
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut header = Vec::new();
    // Read only through the boundary, so body bytes cannot be mistaken for headers.
    while !header.ends_with(b"\r\n\r\n") {
        if header.len() >= 8192 {
            return Err(431);
        }
        let mut byte = [0u8; 1];
        read_deadline(stream, &mut byte, deadline)?;
        header.push(byte.first().copied().ok_or(400u16)?);
    }
    let text = std::str::from_utf8(&header).map_err(|_| 400u16)?;
    let mut lines = text.split("\r\n");
    let parts: Vec<_> = lines.next().ok_or(400u16)?.split(' ').collect();
    let [method, path, "HTTP/1.1"] = parts.as_slice() else { return Err(404) };
    let control = match *path {
        "/authorize" => false,
        "/authorize/control" => true,
        _ => return Err(404),
    };
    if !matches!(*method, "POST" | "OPTIONS") {
        return Err(405);
    }
    let mut headers = BTreeMap::new();
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line.split_once(':').ok_or(400u16)?;
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') || value.chars().any(|c| c.is_control() && c != '\t') {
            return Err(400);
        }
        if headers.insert(name.to_ascii_lowercase(), value.trim().to_owned()).is_some() {
            return Err(400);
        }
    }
    if !headers.contains_key("host") || headers.contains_key("transfer-encoding") || headers.contains_key("expect") {
        return Err(400);
    }
    let size = match headers.get("content-length") {
        Some(value) if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) => value.parse::<usize>().map_err(|_| 413u16)?,
        Some(_) => return Err(400),
        None if *method == "OPTIONS" => 0,
        None => return Err(411),
    };
    if size > if control { crate::cloud_records::b2_control::REQUEST_LIMIT } else { 16 << 10 } {
        return Err(413);
    }
    let mut body = vec![0u8; size];
    read_deadline(stream, &mut body, deadline)?;
    Ok(Request { control, method: (*method).into(), headers, body })
}
fn same_token(expected: &str, actual: &str) -> bool {
    let Ok(mut left) = Hmac::<Sha256>::new_from_slice(b"LightCraft signer session token") else { return false };
    let Ok(mut right) = Hmac::<Sha256>::new_from_slice(b"LightCraft signer session token") else { return false };
    left.update(expected.as_bytes());
    right.update(actual.as_bytes());
    left.verify_slice(&right.finalize().into_bytes()).is_ok()
}
fn response(stream: &mut TcpStream, status: u16, origin: Option<&str>, body: &[u8]) {
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        409 => "Conflict",
        422 => "Unprocessable Content",
        411 => "Length Required",
        413 => "Content Too Large",
        431 => "Request Header Fields Too Large",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    };
    let cors=origin.map(|origin|format!("Access-Control-Allow-Origin: {origin}\r\nAccess-Control-Allow-Methods: POST\r\nAccess-Control-Allow-Headers: authorization, content-type\r\nAccess-Control-Max-Age: 300\r\nVary: Origin\r\n")).unwrap_or_default();
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n{cors}\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes()).and_then(|()| stream.write_all(body));
}
fn handle(stream: TcpStream, config: &Config, token: &str) {
    handle_using(stream, config, token, |request| control::execute(&config.connection, request));
}
fn handle_using(
    mut stream: TcpStream,
    config: &Config,
    token: &str,
    execute: impl FnOnce(&crate::cloud_records::b2_control::Request) -> Result<crate::cloud_records::b2_control::Reply, crate::cloud_status::CloudFailure>,
) {
    let r = match read_request(&mut stream) {
        Ok(r) => r,
        Err(status) => {
            response(&mut stream, status, None, b"{\"error\":\"invalid HTTP request\"}");
            return;
        }
    };
    if r.headers.get("origin") != Some(&config.origin) {
        response(&mut stream, 403, None, b"{\"error\":\"browser origin is not allowed\"}");
        return;
    }
    let origin = Some(config.origin.as_str());
    if r.method == "OPTIONS" {
        let headers = r.headers.get("access-control-request-headers").map(String::as_str).unwrap_or("");
        if r.headers.get("access-control-request-method").map(String::as_str) != Some("POST")
            || !r.body.is_empty()
            || headers.split(',').any(|header| !matches!(header.trim().to_ascii_lowercase().as_str(), "authorization" | "content-type" | ""))
        {
            response(&mut stream, 403, origin, b"{\"error\":\"preflight is outside signer scope\"}");
        } else {
            response(&mut stream, 204, origin, b"");
        }
        return;
    }
    let actual = r.headers.get("authorization").and_then(|v| v.strip_prefix("Bearer ")).unwrap_or("");
    if !same_token(token, actual) {
        response(&mut stream, 401, origin, b"{\"error\":\"signer session token is missing or invalid\"}");
        return;
    }
    if r.headers.get("content-type").and_then(|v| v.split(';').next()).map(str::trim) != Some("application/json") {
        response(&mut stream, 400, origin, b"{\"error\":\"expected application/json\"}");
        return;
    }
    if r.control {
        use crate::cloud_records::b2_control::{RESPONSE_LIMIT, Reply, Request};
        let Ok(request) = serde_json::from_slice::<Request>(&r.body) else {
            response(&mut stream, 400, origin, b"{\"error\":\"invalid control request\"}");
            return;
        };
        let result = control::validate(&config.connection, &request).and_then(|()| execute(&request));
        let (status, reply) = match result {
            Ok(reply) => (200, reply),
            Err(failure) => (control::status(&failure), Reply::Failed { failure }),
        };
        match serde_json::to_vec(&reply) {
            Ok(bytes) if bytes.len() <= RESPONSE_LIMIT => response(&mut stream, status, origin, &bytes),
            _ => response(&mut stream, 500, origin, b"{\"error\":\"could not encode control response\"}"),
        }
        return;
    }
    let Ok(request) = serde_json::from_slice::<BrowserRequest>(&r.body) else {
        response(&mut stream, 400, origin, b"{\"error\":\"invalid authorization request\"}");
        return;
    };
    match config.connection.authorize_browser(&request, config.expires_seconds) {
        Ok(result) => match serde_json::to_vec(&result) {
            Ok(bytes) => response(&mut stream, 200, origin, &bytes),
            Err(_) => response(&mut stream, 500, origin, b"{\"error\":\"could not encode authorization\"}"),
        },
        Err(crate::assets::AssetError::Invalid(_)) => response(&mut stream, 403, origin, b"{\"error\":\"request is outside signer scope\"}"),
        Err(_) => response(&mut stream, 503, origin, b"{\"error\":\"signer credentials unavailable; operator must repair server configuration\"}"),
    }
}
/// Serve one personal scoped connection. AWS credentials remain on this host; the
/// separate session token grants browser reads and metadata editing within its prefix.
pub fn serve(config: Config, listen: SocketAddr, token: String, max_connections: usize) -> Result<(), String> {
    config.validate()?;
    if !listen.ip().is_loopback() {
        return Err("signer must listen on loopback; put an HTTPS reverse proxy in front of it".into());
    }
    if !(32..=4096).contains(&token.len())
        || !token.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'+' | b'/' | b'='))
        || !(1..=64).contains(&max_connections)
    {
        return Err("signer needs a random session token of 32–4096 characters and 1–64 concurrent connections".into());
    }
    let listener = TcpListener::bind(listen).map_err(|e| format!("cannot bind signer: {e}"))?;
    let config = Arc::new(config);
    let token = Arc::new(token);
    let active = Arc::new(AtomicUsize::new(0));
    eprintln!(
        "LightCraft personal signer listening on {} (loopback; requests and signed URLs are not logged)",
        listener.local_addr().map_err(|e| e.to_string())?
    );
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        if active.fetch_add(1, Ordering::AcqRel) >= max_connections {
            active.fetch_sub(1, Ordering::AcqRel);
            let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
            response(&mut stream, 503, None, b"{\"error\":\"signer busy; retry later\"}");
            continue;
        }
        struct Active(Arc<AtomicUsize>);
        impl Drop for Active {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::AcqRel);
            }
        }
        let slot = Active(active.clone());
        let config = config.clone();
        let token = token.clone();
        let _ = std::thread::Builder::new().name("cloud-signer".into()).spawn(move || {
            let _slot = slot;
            handle(stream, &config, &token);
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn config() -> Config {
        serde_json::from_value(json!({"connection":{"id":"personal","bucket":"photos","region":"us-east-1","prefix":"library","endpoint":"http://127.0.0.1:9000"},"origin":"http://127.0.0.1:8080"})).unwrap()
    }
    fn exchange(text: &str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let task = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut config = config();
            config.connection = config.connection.with_keys(crate::assets::s3::AccessKeys::new("AKIDEXAMPLE", "secret-example").unwrap());
            handle(stream, &config, "EXAMPLE_SESSION_TOKEN_32_CHARACTERS");
        });
        let mut stream = TcpStream::connect(address).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        stream.write_all(text.as_bytes()).unwrap();
        stream.shutdown(std::net::Shutdown::Write).unwrap();
        let mut result = String::new();
        stream.read_to_string(&mut result).unwrap();
        task.join().unwrap();
        result
    }
    fn post(origin: &str, token: &str, body: &str) -> String {
        exchange(&format!(
            "POST /authorize HTTP/1.1\r\nHost: localhost\r\nOrigin: {origin}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ))
    }
    #[test]
    fn personal_signer_http_auth_cors_and_scope_are_enforced() {
        let origin = "http://127.0.0.1:8080";
        let token = "EXAMPLE_SESSION_TOKEN_32_CHARACTERS";
        let body = json!({"connection":"personal","method":"GET","key":"library/originals/x"}).to_string();
        let ok = post(origin, token, &body);
        assert!(ok.starts_with("HTTP/1.1 200"));
        assert!(ok.contains("Access-Control-Allow-Origin: http://127.0.0.1:8080"));
        assert!(ok.contains("X-Amz-Signature"));
        assert!(!ok.contains("secret-example") && !ok.contains(token));
        let denied = post("https://other.example", token, &body);
        assert!(denied.starts_with("HTTP/1.1 403") && !denied.contains("Access-Control-Allow-Origin"));
        assert!(post(origin, "wrong", &body).starts_with("HTTP/1.1 401"));
        let outside = json!({"connection":"personal","method":"GET","key":"other/originals/x"}).to_string();
        assert!(post(origin, token, &outside).starts_with("HTTP/1.1 403"));
        let preflight = exchange(&format!(
            "OPTIONS /authorize HTTP/1.1\r\nHost: localhost\r\nOrigin: {origin}\r\nAccess-Control-Request-Method: POST\r\nAccess-Control-Request-Headers: authorization, content-type\r\n\r\n"
        ));
        assert!(preflight.starts_with("HTTP/1.1 204"));
        assert!(!preflight.contains("Allow-Credentials"));
    }
    #[test]
    fn personal_signer_rejects_ambiguous_or_oversized_http_and_open_configuration() {
        for headers in ["Content-Length: 0\r\nContent-Length: 0", "Content-Length: 0\r\nTransfer-Encoding: chunked"] {
            assert!(exchange(&format!("POST /authorize HTTP/1.1\r\nHost: localhost\r\n{headers}\r\n\r\n")).starts_with("HTTP/1.1 400"));
        }
        assert!(exchange("POST /authorize HTTP/1.1\r\nHost: localhost\r\nContent-Length: 16385\r\n\r\n").starts_with("HTTP/1.1 413"));
        assert!(same_token("token", "token"));
        assert!(!same_token("token", "tokeN"));
        assert!(!same_token("token", "token-longer"));
        let mut c = config();
        c.validate().unwrap();
        c.origin = "*".into();
        assert!(c.validate().is_err());
        c = config();
        c.connection.prefix.clear();
        assert!(c.validate().is_err());
        assert!(serve(config(), "0.0.0.0:0".parse().unwrap(), "EXAMPLE_SESSION_TOKEN_32_CHARACTERS".into(), 1).unwrap_err().contains("loopback"));
    }
}
