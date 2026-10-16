use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::Duration;
struct Signer {
    child: Child,
    dir: std::path::PathBuf,
    address: String,
}
impl Drop for Signer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
impl Signer {
    fn start() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "lightcraft-signer-cli-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir(&dir).unwrap();
        let config = dir.join("signer.json");
        std::fs::write(&config,json!({"connection":{"id":"personal","bucket":"photos","region":"us-east-1","prefix":"library","endpoint":"http://127.0.0.1:9000"},"origin":"http://127.0.0.1:8080"}).to_string()).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_lightcraft-cli"))
            .args(["cloud-signer", "--config"])
            .arg(config)
            .args(["--listen", "127.0.0.1:0"])
            .env("LIGHTCRAFT_SIGNER_TOKEN", "EXAMPLE_SESSION_TOKEN_32_CHARACTERS")
            .env("AWS_ACCESS_KEY_ID", "AKIDEXAMPLE")
            .env("AWS_SECRET_ACCESS_KEY", "test-secret-example")
            .env("AWS_SESSION_TOKEN", "test-session-example")
            .env_remove("AWS_PROFILE")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut s = Self { child, dir, address: String::new() };
        let stderr = s.child.stderr.take().unwrap();
        let (send, recv) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            let result = reader.read_line(&mut line);
            let _ = send.send((result, line));
        });
        let (result, line) = recv.recv_timeout(Duration::from_secs(15)).unwrap();
        result.unwrap();
        assert!(line.contains("signer listening on"), "{line}");
        s.address = line.split("listening on ").nth(1).unwrap().split_whitespace().next().unwrap().into();
        s
    }
    fn request(&self, token: &str, body: Value) -> (u16, String, Value) {
        let body = body.to_string();
        let mut stream = TcpStream::connect(&self.address).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        write!(stream,"POST /authorize HTTP/1.1\r\nHost: localhost\r\nOrigin: http://127.0.0.1:8080\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",body.len()).unwrap();
        stream.shutdown(Shutdown::Write).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let (headers, body) = response.split_once("\r\n\r\n").unwrap();
        let status = headers.split_whitespace().nth(1).unwrap().parse().unwrap();
        (status, headers.into(), serde_json::from_str(body).unwrap())
    }
}
#[test]
fn maintained_signer_cli_launches_from_configuration_and_enforces_auth_and_scope() {
    let signer = Signer::start();
    let token = "EXAMPLE_SESSION_TOKEN_32_CHARACTERS";
    let read = json!({"connection":"personal","method":"GET","key":"library/originals/example","version":"v+/="});
    let (status, headers, body) = signer.request(token, read.clone());
    assert_eq!(status, 200);
    assert!(headers.contains("Access-Control-Allow-Origin: http://127.0.0.1:8080"));
    assert!(headers.contains("Cache-Control: no-store"));
    let url = body["url"].as_str().unwrap();
    assert!(url.starts_with("http://127.0.0.1:9000/photos/library/originals/example?"));
    assert!(url.contains("versionId=v%2B%2F%3D") && url.contains("X-Amz-Signature="));
    assert!(!body.to_string().contains("test-secret-example") && !body.to_string().contains(token));
    assert_eq!(signer.request("wrong", read).0, 401);
    assert_eq!(signer.request(token, json!({"connection":"personal","method":"DELETE","key":"library/originals/example"})).0, 403);
    assert_eq!(signer.request(token, json!({"connection":"personal","method":"GET","key":"other/originals/example"})).0, 403);
}
