//! Native S3 reads over the existing RustCrypto HTTPS transport, with AWS SigV4 signing.
//! Connection settings contain no secrets. Credentials are resolved for each request.

use super::{AssetError, BlobStore, Result};
use hmac::{Hmac, Mac};
use lightcraft_catalog::assets::Blob;
use lightcraft_fetch::http::{self, Limits, Url};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct S3Connection {
    pub id: String,
    pub bucket: String,
    pub region: String,
    /// All keys are constrained to this library prefix. Empty permits the configured bucket.
    #[serde(default)]
    pub prefix: String,
    /// An S3-compatible endpoint uses path-style bucket addressing. None uses AWS virtual hosting.
    #[serde(default)]
    pub endpoint: Option<String>,
    /// Explicit profile, otherwise AWS environment credentials and then the default profile.
    #[serde(default)]
    pub profile: Option<String>,
}

impl S3Connection {
    pub fn validate(&self) -> Result<()> {
        if !lightcraft_catalog::assets::identifier(&self.id)
            || self.bucket.len() < 3
            || self.bucket.len() > 63
            || !self.bucket.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'.'))
            || self.region.is_empty()
            || self.region.len() > 64
            || !self.region.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(AssetError::Invalid("invalid connection, bucket, or region".into()));
        }
        if !self.prefix.is_empty() {
            let dummy = Blob {
                id: lightcraft_catalog::assets::BlobId("a".repeat(64)),
                size: 1,
                remote: lightcraft_catalog::assets::RemoteLocation {
                    connection: self.id.clone(),
                    key: self.prefix.trim_end_matches('/').into(),
                    version: None,
                },
            };
            dummy.validate().map_err(AssetError::Invalid)?;
        }
        if self.profile.as_ref().is_some_and(|p| p.is_empty() || p.len() > 128 || p.chars().any(char::is_control)) {
            return Err(AssetError::Invalid("invalid AWS profile name".into()));
        }
        if let Some(endpoint) = &self.endpoint {
            let url = Url::parse(endpoint).map_err(|_| AssetError::Invalid("invalid S3 endpoint".into()))?;
            if endpoint.contains(['?', '#']) || url.path != "/" || (!url.tls && !matches!(url.host.as_str(), "127.0.0.1" | "::1" | "localhost")) {
                return Err(AssetError::Invalid(
                    "S3 endpoints must use HTTPS (HTTP allowed only for loopback test servers) and have no path or query".into(),
                ));
            }
        }
        Ok(())
    }

    fn url(&self, blob: &Blob) -> Result<Url> {
        self.validate()?;
        blob.validate().map_err(AssetError::Invalid)?;
        let prefix = self.prefix.trim_end_matches('/');
        if blob.remote.connection != self.id || (!prefix.is_empty() && !blob.remote.key.starts_with(&format!("{prefix}/"))) {
            return Err(AssetError::Invalid("object is outside the configured connection or library prefix".into()));
        }
        let base = match &self.endpoint {
            Some(endpoint) => format!("{}/{}", endpoint.trim_end_matches('/'), self.bucket),
            None => format!("https://{}.s3.{}.amazonaws.com", self.bucket, self.region),
        };
        let mut full = format!("{}/{}", base, encode(&blob.remote.key, true));
        if let Some(version) = &blob.remote.version {
            full.push_str(&format!("?versionId={}", encode(version, false)));
        }
        Url::parse(&full).map_err(|_| AssetError::Invalid("invalid object URL".into()))
    }
}

/// Never derives Debug/Serialize: access keys and tokens must not enter logs or settings.
struct Credentials {
    access: String,
    secret: String,
    token: Option<String>,
}

impl Credentials {
    fn load(profile: Option<&str>) -> Result<Self> {
        if profile.is_none()
            && let (Ok(access), Ok(secret)) = (std::env::var("AWS_ACCESS_KEY_ID"), std::env::var("AWS_SECRET_ACCESS_KEY"))
        {
            return Self::checked(access, secret, std::env::var("AWS_SESSION_TOKEN").ok());
        }
        let path = std::env::var_os("AWS_SHARED_CREDENTIALS_FILE")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(|p| std::path::PathBuf::from(p).join(".aws/credentials"))
            })
            .ok_or_else(|| AssetError::Unconfigured("set AWS temporary credentials or configure an AWS shared credentials profile".into()))?;
        let file = std::fs::File::open(path).map_err(|_| AssetError::Unconfigured("AWS shared credentials file could not be read".into()))?;
        if file.metadata().map_err(|_| AssetError::Unauthorized)?.len() > 1 << 20 {
            return Err(AssetError::Invalid("AWS credentials file exceeds 1 MiB".into()));
        }
        let mut text = String::new();
        file.take((1 << 20) + 1).read_to_string(&mut text).map_err(|_| AssetError::Unauthorized)?;
        let default_profile = std::env::var("AWS_PROFILE").unwrap_or_else(|_| "default".into());
        Self::parse_profile(&text, profile.unwrap_or(&default_profile))
    }

    fn parse_profile(text: &str, wanted: &str) -> Result<Self> {
        let mut active = false;
        let (mut access, mut secret, mut token) = (None, None, None);
        for line in text.lines().map(str::trim).filter(|l| !l.starts_with(['#', ';'])) {
            if line.starts_with('[') {
                active = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')).is_some_and(|s| s.trim() == wanted);
                continue;
            }
            if active && let Some((key, value)) = line.split_once('=') {
                let value = value.trim().to_string();
                match key.trim() {
                    "aws_access_key_id" => access = Some(value),
                    "aws_secret_access_key" => secret = Some(value),
                    "aws_session_token" => token = Some(value),
                    _ => {}
                }
            }
        }
        Self::checked(
            access.ok_or_else(|| {
                AssetError::Unconfigured("AWS profile has no access key; use shared temporary credentials or environment credentials".into())
            })?,
            secret.ok_or_else(|| AssetError::Unconfigured("AWS profile has no secret key".into()))?,
            token,
        )
    }

    fn checked(access: String, secret: String, token: Option<String>) -> Result<Self> {
        if access.is_empty()
            || secret.is_empty()
            || access.len() > 256
            || secret.len() > 256
            || access.chars().any(char::is_control)
            || secret.chars().any(char::is_control)
            || token.as_ref().is_some_and(|t| t.is_empty() || t.len() > 16 << 10 || t.chars().any(char::is_control))
        {
            return Err(AssetError::Invalid("invalid AWS credentials".into()));
        }
        Ok(Self { access, secret, token })
    }
}

fn encode(s: &str, slash: bool) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') || (slash && b == b'/') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn mac(key: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    let mut h = Hmac::<Sha256>::new_from_slice(key).map_err(|_| AssetError::Invalid("could not initialize request signing".into()))?;
    h.update(data);
    Ok(h.finalize().into_bytes().to_vec())
}

fn signing_headers(url: &Url, region: &str, credentials: &Credentials, timestamp: &str) -> Result<Vec<(&'static str, String)>> {
    if timestamp.len() != 16 || !timestamp.get(..8).is_some_and(|s| s.bytes().all(|b| b.is_ascii_digit())) {
        return Err(AssetError::Invalid("invalid request date".into()));
    }
    let day = timestamp.get(..8).ok_or_else(|| AssetError::Invalid("invalid request date".into()))?;
    let host = if url.host.contains(':') { format!("[{}]", url.host) } else { url.host.clone() };
    let host = if url.port == if url.tls { 443 } else { 80 } { host } else { format!("{host}:{}", url.port) };
    let payload = super::sha256(b"").0;
    let mut headers = vec![("host", host), ("x-amz-content-sha256", payload.clone()), ("x-amz-date", timestamp.into())];
    if let Some(token) = &credentials.token {
        headers.push(("x-amz-security-token", token.clone()));
    }
    let signed = headers.iter().map(|(k, _)| *k).collect::<Vec<_>>().join(";");
    let canonical_headers = headers.iter().map(|(k, v)| format!("{k}:{}\n", v.split_whitespace().collect::<Vec<_>>().join(" "))).collect::<String>();
    let (path, query) = url.path.split_once('?').unwrap_or((&url.path, ""));
    let canonical = format!("GET\n{path}\n{query}\n{canonical_headers}\n{signed}\n{payload}");
    let scope = format!("{day}/{region}/s3/aws4_request");
    let string_to_sign = format!("AWS4-HMAC-SHA256\n{timestamp}\n{scope}\n{:x}", Sha256::digest(canonical.as_bytes()));
    let date_key = mac(format!("AWS4{}", credentials.secret).as_bytes(), day.as_bytes())?;
    let region_key = mac(&date_key, region.as_bytes())?;
    let service_key = mac(&region_key, b"s3")?;
    let signing_key = mac(&service_key, b"aws4_request")?;
    let signature = mac(&signing_key, string_to_sign.as_bytes())?.iter().map(|b| format!("{b:02x}")).collect::<String>();
    headers.retain(|(k, _)| *k != "host");
    headers.push((
        "Authorization",
        format!("AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed}, Signature={signature}", credentials.access),
    ));
    Ok(headers)
}

impl BlobStore for S3Connection {
    fn download(&self, blob: &Blob, out: &mut dyn Write, cancel: &AtomicBool, progress: &mut dyn FnMut(u64)) -> Result<()> {
        let url = self.url(blob)?;
        let credentials = Credentials::load(self.profile.as_deref())?;
        let now = time::OffsetDateTime::now_utc();
        let timestamp =
            format!("{:04}{:02}{:02}T{:02}{:02}{:02}Z", now.year(), u8::from(now.month()), now.day(), now.hour(), now.minute(), now.second());
        let headers = signing_headers(&url, &self.region, &credentials, &timestamp)?;
        let limits = Limits { connect: Duration::from_secs(15), stall: Duration::from_secs(30), cancel };
        let mut response = http::get(&url, &headers, &limits).map_err(http_error)?;
        match response.status {
            200 => {}
            401 | 403 => return Err(AssetError::Unauthorized),
            404 => return Err(AssetError::Missing),
            status => return Err(AssetError::Network(format!("S3 returned HTTP {status}; check the endpoint and region"))),
        }
        if response.content_length().is_some_and(|n| n != blob.size) {
            return Err(AssetError::Integrity);
        }
        let mut buf = [0u8; 64 << 10];
        let mut done = 0u64;
        loop {
            let n = response.read(&mut buf, &limits).map_err(http_error)?;
            if n == 0 {
                break;
            }
            done = done.saturating_add(n as u64);
            if done > blob.size {
                return Err(AssetError::Integrity);
            }
            out.write_all(buf.get(..n).ok_or(AssetError::Integrity)?).map_err(super::disk)?;
            progress(done);
        }
        Ok(())
    }
}

fn http_error(error: http::HttpError) -> AssetError {
    match error {
        http::HttpError::Cancelled => AssetError::Cancelled,
        _ => AssetError::Network(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hmac_matches_rfc4231() {
        let bytes = mac(&[0x0b; 20], b"Hi There").unwrap();
        let hex = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
        assert_eq!(hex, "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7");
    }
    #[test]
    fn keys_stay_in_prefix_and_are_encoded_once() {
        let config = S3Connection {
            id: "test".into(),
            bucket: "photos".into(),
            region: "us-east-1".into(),
            prefix: "library".into(),
            endpoint: Some("http://127.0.0.1:9000".into()),
            profile: None,
        };
        let mut blob = Blob {
            id: super::super::sha256(b"x"),
            size: 1,
            remote: lightcraft_catalog::assets::RemoteLocation {
                connection: "test".into(),
                key: "library/café 1%.raw".into(),
                version: Some("a+/=".into()),
            },
        };
        assert_eq!(config.url(&blob).unwrap().path, "/photos/library/caf%C3%A9%201%25.raw?versionId=a%2B%2F%3D");
        blob.remote.key = "library-other/photo".into();
        assert!(config.url(&blob).is_err());
    }
    #[test]
    fn profile_parsing_does_not_mix_sections() {
        let text = "[wrong]\naws_access_key_id=wrong\naws_secret_access_key=wrong\n[chosen]\naws_access_key_id=access\naws_secret_access_key=secret\naws_session_token=temporary\n";
        let c = Credentials::parse_profile(text, "chosen").unwrap();
        assert_eq!(c.access, "access");
        assert_eq!(c.token.as_deref(), Some("temporary"));
        assert!(Credentials::parse_profile(text, "absent").is_err());
    }
}
