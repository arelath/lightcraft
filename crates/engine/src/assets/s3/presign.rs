//! Scoped browser SigV4 authorization. Returned URLs are bearer capabilities; never log them.
use super::*;
use std::collections::BTreeMap;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserRequest {
    pub connection: String,
    pub method: String,
    pub key: String,
    pub version: Option<String>,
    pub condition: Option<String>,
    pub size: Option<u64>,
    pub content_sha256: Option<String>,
    pub operation: Option<String>,
    pub list: Option<crate::cloud_append::listing::ListRequest>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserAuthorization {
    pub url: String,
    pub expires_at: u64,
    pub headers: BTreeMap<String, String>,
}
fn denied() -> AssetError {
    AssetError::Invalid("request is outside this signer's read/metadata-write scope".into())
}
fn hash(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
}
fn identifier(s: &str) -> bool {
    lightcraft_catalog::assets::identifier(s)
}
/// A canonical post-rebase epoch directory (`catalog/epochs/<n>`, n >= 2; see `cloud_control::epoch_root`).
fn epoch(s: &str) -> bool {
    s.parse::<u64>().is_ok_and(|epoch| epoch >= 2 && epoch.to_string() == s)
}
fn write_limit(key: &str, digest: &str, immutable: bool, b2: bool) -> Option<u64> {
    let parts: Vec<_> = key.split('/').collect();
    let named = format!("{digest}.json");
    match parts.as_slice() {
        ["catalog", "head.json"] if !immutable => Some(16 << 10),
        ["catalog", "changes", file] if (!immutable || b2) && *file == named => Some(2 << 20),
        ["catalog", "checkpoints", file] if (!immutable || b2) && *file == named => Some(64 << 20),
        ["catalog", "epochs", n, "changes", file] if (!immutable || b2) && epoch(n) && *file == named => Some(2 << 20),
        ["catalog", "epochs", n, "checkpoints", file] if (!immutable || b2) && epoch(n) && *file == named => Some(64 << 20),
        ["catalog", "legacy", file] if immutable && b2 && *file == named => Some(crate::cloud_records::MAX_INTAKE),
        ["sync-v2", library, "receipts", file] if identifier(library) && *file == named => Some(1024),
        ["sync-v2", library, "manifest", file] if immutable && identifier(library) && *file == named => Some(64 << 10),
        ["sync-v2", library, "checkpoints", author, file] if immutable && identifier(library) && identifier(author) && *file == named => {
            Some(64 << 20)
        }
        ["sync-v2", library, "ops", author, file] if immutable && identifier(library) && identifier(author) => {
            let (sequence, tail) = file.split_once('-')?;
            (sequence.len() == 20 && sequence.bytes().all(|b| b.is_ascii_digit()) && sequence.parse::<u64>().ok()? > 0 && tail == named)
                .then_some(2 << 20)
        }
        _ => None,
    }
}
fn listing_scope(prefix: &str) -> bool {
    let parts: Vec<_> = prefix.split('/').collect();
    matches!(parts.as_slice(), ["sync-v2", library, "ops" | "checkpoints" | "control", ""] if identifier(library))
        || matches!(parts.as_slice(), ["sync-v2", library, "ops", author, ""] if identifier(library) && identifier(author))
}
// Shared strict SHA-256-to-base64 conversion; signer failures keep their scope diagnosis.
fn checksum(hex: &str) -> Result<String> {
    super::checksum::base64_digest(hex).map_err(|_| denied())
}

impl S3Connection {
    /// Personal signer scope: reads under a dedicated prefix, bounded metadata writes,
    /// append discovery only. Media upload, deletion and arbitrary listing are not granted.
    pub fn authorize_browser(&self, request: &BrowserRequest, ttl_seconds: u64) -> Result<BrowserAuthorization> {
        self.authorize_browser_at(request, ttl_seconds, time::OffsetDateTime::now_utc())
    }
    fn authorize_browser_at(&self, r: &BrowserRequest, ttl: u64, now: time::OffsetDateTime) -> Result<BrowserAuthorization> {
        self.validate()?;
        if self.prefix.is_empty() || self.prefix.ends_with('/') || self.prefix.len() > 1024 || r.connection != self.id || !(30..=900).contains(&ttl) {
            return Err(denied());
        }
        let immutable = r.operation.as_deref() == Some("putImmutable");
        let listing = r.operation.as_deref() == Some("list");
        if r.operation.is_some() && !immutable && !listing {
            return Err(denied());
        }
        let mut query = BTreeMap::<String, String>::new();
        let mut headers = BTreeMap::<String, String>::new();
        let url = if listing {
            let list = r.list.as_ref().ok_or_else(denied)?;
            list.validate(&self.prefix)?;
            let relative = list.prefix.strip_prefix(&format!("{}/", self.prefix)).ok_or_else(denied)?;
            if r.method != "GET"
                || !r.key.is_empty()
                || r.version.is_some()
                || r.condition.is_some()
                || r.size.is_some()
                || r.content_sha256.is_some()
                || !listing_scope(relative)
            {
                return Err(denied());
            }
            query.insert("list-type".into(), "2".into());
            query.insert("max-keys".into(), "1000".into());
            query.insert("prefix".into(), list.prefix.clone());
            if list.delimiter {
                query.insert("delimiter".into(), "/".into());
            }
            if let Some(value) = &list.continuation {
                query.insert("continuation-token".into(), value.clone());
            }
            if let Some(value) = &list.start_after {
                query.insert("start-after".into(), value.clone());
            }
            let base = match &self.endpoint {
                Some(endpoint) => format!("{}/{}/", endpoint.trim_end_matches('/'), self.bucket),
                None => format!("https://{}.s3.{}.amazonaws.com/", self.bucket, self.region),
            };
            Url::parse(&base).map_err(|_| denied())?
        } else {
            if r.list.is_some() {
                return Err(denied());
            }
            let blob = Blob {
                id: super::super::sha256(b"scope"),
                size: 1,
                remote: lightcraft_catalog::assets::RemoteLocation {
                    connection: r.connection.clone(),
                    key: r.key.clone(),
                    version: r.version.clone(),
                },
            };
            let url = self.url(&blob)?;
            if let Some(version) = &r.version {
                query.insert("versionId".into(), version.clone());
            }
            match r.method.as_str() {
                "GET" | "HEAD" if !immutable && r.condition.is_none() && r.size.is_none() && r.content_sha256.is_none() => {}
                "PUT" if r.version.is_none() => {
                    let digest = r.content_sha256.as_deref().filter(|s| hash(s)).ok_or_else(denied)?;
                    let relative = r.key.strip_prefix(&format!("{}/", self.prefix)).ok_or_else(denied)?;
                    let b2 =
                        self.endpoint.as_deref().and_then(crate::cloud_setup::b2_runtime_endpoint).is_some_and(|(_, region)| region == self.region);
                    let limit = write_limit(relative, digest, immutable, b2).ok_or_else(denied)?;
                    if r.size.is_none_or(|size| size == 0 || size > limit) {
                        return Err(denied());
                    }
                    if immutable {
                        if r.condition.is_some() {
                            return Err(denied());
                        }
                    } else {
                        let condition = r.condition.as_deref().ok_or_else(denied)?;
                        if condition == "*" {
                            headers.insert("if-none-match".into(), "*".into());
                        } else if relative == "catalog/head.json"
                            && condition.len() >= 2
                            && condition.len() <= 256
                            && condition.starts_with('"')
                            && condition.ends_with('"')
                            && condition.bytes().all(|b| matches!(b, 0x21..=0x7e))
                        {
                            headers.insert("if-match".into(), condition.into());
                        } else {
                            return Err(denied());
                        }
                    }
                    headers.insert("x-amz-checksum-sha256".into(), checksum(digest)?);
                }
                _ => return Err(denied()),
            }
            url
        };
        let credentials = self.credentials()?;
        let timestamp =
            format!("{:04}{:02}{:02}T{:02}{:02}{:02}Z", now.year(), u8::from(now.month()), now.day(), now.hour(), now.minute(), now.second());
        let day = timestamp.get(..8).ok_or_else(denied)?;
        let scope = format!("{day}/{}/s3/aws4_request", self.region);
        let host = if url.host.contains(':') { format!("[{}]", url.host) } else { url.host.clone() };
        let host = if url.port == if url.tls { 443 } else { 80 } { host } else { format!("{host}:{}", url.port) };
        let mut signed_headers = headers.clone();
        if r.method == "PUT" {
            signed_headers.insert("content-length".into(), r.size.ok_or_else(denied)?.to_string());
        }
        signed_headers.insert("host".into(), host);
        let names = signed_headers.keys().cloned().collect::<Vec<_>>().join(";");
        query.insert("X-Amz-Algorithm".into(), "AWS4-HMAC-SHA256".into());
        query.insert("X-Amz-Credential".into(), format!("{}/{scope}", credentials.access));
        query.insert("X-Amz-Date".into(), timestamp.clone());
        query.insert("X-Amz-Expires".into(), ttl.to_string());
        query.insert("X-Amz-SignedHeaders".into(), names.clone());
        if let Some(token) = &credentials.token {
            query.insert("X-Amz-Security-Token".into(), token.clone());
        }
        let mut encoded: Vec<_> = query.iter().map(|(k, v)| (encode(k, false), encode(v, false))).collect();
        encoded.sort();
        let query = encoded.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&");
        let path = url.path.split('?').next().ok_or_else(denied)?;
        let canonical_headers =
            signed_headers.iter().map(|(k, v)| format!("{k}:{}\n", v.split_whitespace().collect::<Vec<_>>().join(" "))).collect::<String>();
        let canonical = format!("{}\n{path}\n{query}\n{canonical_headers}\n{names}\nUNSIGNED-PAYLOAD", r.method);
        let to_sign = format!("AWS4-HMAC-SHA256\n{timestamp}\n{scope}\n{:x}", Sha256::digest(canonical.as_bytes()));
        let date_key = mac(format!("AWS4{}", credentials.secret).as_bytes(), day.as_bytes())?;
        let region_key = mac(&date_key, self.region.as_bytes())?;
        let service_key = mac(&region_key, b"s3")?;
        let key = mac(&service_key, b"aws4_request")?;
        let signature = mac(&key, to_sign.as_bytes())?.iter().map(|b| format!("{b:02x}")).collect::<String>();
        let origin = match &self.endpoint {
            Some(endpoint) => endpoint.trim_end_matches('/').to_owned(),
            None => format!("https://{}.s3.{}.amazonaws.com", self.bucket, self.region),
        };
        let expires_at = u64::try_from(now.unix_timestamp()).map_err(|_| denied())?.saturating_add(ttl).saturating_mul(1000);
        Ok(BrowserAuthorization { url: format!("{origin}{path}?{query}&X-Amz-Signature={signature}"), expires_at, headers })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn connection() -> S3Connection {
        serde_json::from_value::<S3Connection>(
            json!({"id":"personal","bucket":"photos","region":"us-east-1","prefix":"library","endpoint":"http://127.0.0.1:9000"}),
        )
        .unwrap()
        .with_keys(AccessKeys::new("AKIDEXAMPLE", "test-secret").unwrap())
    }
    fn request(value: serde_json::Value) -> BrowserRequest {
        serde_json::from_value(value).unwrap()
    }
    #[test]
    fn signer_binds_exact_hash_condition_scope_and_lifetime() {
        let c = connection();
        let digest = super::super::super::sha256(b"metadata").0;
        let mut r = request(
            json!({"connection":"personal","method":"PUT","key":"library/catalog/head.json","condition":"\"observed\"","size":8,"contentSha256":digest}),
        );
        let now = time::OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();
        let signed = c.authorize_browser_at(&r, 300, now).unwrap();
        assert_eq!(signed.expires_at, 1_800_000_300_000);
        assert_eq!(signed.headers["if-match"], "\"observed\"");
        assert_eq!(signed.headers["x-amz-checksum-sha256"], checksum(&digest).unwrap());
        assert!(signed.url.contains("X-Amz-SignedHeaders=content-length%3Bhost%3Bif-match%3Bx-amz-checksum-sha256"));
        assert!(!signed.url.contains("test-secret"));
        assert!(c.authorize_browser_at(&r, 901, now).is_err());
        r.key = "library-other/catalog/head.json".into();
        assert!(c.authorize_browser(&r, 300).is_err());
        r.key = "library/catalog/changes/".to_owned() + &"a".repeat(64) + ".json";
        r.condition = Some("*".into());
        assert!(c.authorize_browser(&r, 300).is_err());
        r.key = format!("library/catalog/changes/{digest}.json");
        assert!(c.authorize_browser(&r, 300).is_ok());
        r.condition = Some("\"etag\"".into());
        assert!(c.authorize_browser(&r, 300).is_err());
        // Rebased libraries publish into canonical epoch namespaces (CR.42).
        r.condition = Some("*".into());
        for (key, ok) in [
            (format!("library/catalog/epochs/2/changes/{digest}.json"), true),
            (format!("library/catalog/epochs/17/checkpoints/{digest}.json"), true),
            (format!("library/catalog/epochs/1/changes/{digest}.json"), false),
            (format!("library/catalog/epochs/02/changes/{digest}.json"), false),
            (format!("library/catalog/epochs/x/changes/{digest}.json"), false),
            ("library/catalog/epochs/2/head.json".to_owned(), false),
        ] {
            r.key = key;
            assert_eq!(c.authorize_browser(&r, 300).is_ok(), ok, "{}", r.key);
        }
    }
    #[test]
    fn signer_rejects_media_writes_deletes_unscoped_lists_and_contradictory_fields() {
        let c = connection();
        let digest = super::super::super::sha256(b"x").0;
        for method in ["PUT", "DELETE", "POST"] {
            let r = request(
                json!({"connection":"personal","method":method,"key":format!("library/originals/{digest}"),"condition":"*","size":1,"contentSha256":digest}),
            );
            assert!(c.authorize_browser(&r, 300).is_err());
        }
        let mut r = request(
            json!({"connection":"personal","method":"GET","key":"","operation":"list","list":{"prefix":"library/sync-v2/lib/ops/","delimiter":true}}),
        );
        assert!(c.authorize_browser(&r, 300).is_ok());
        r.list.as_mut().unwrap().prefix = "library/sync-v2/lib/control/".into();
        assert!(c.authorize_browser(&r, 300).is_ok());
        r.list.as_mut().unwrap().prefix = "library/".into();
        assert!(c.authorize_browser(&r, 300).is_err());
        r = request(json!({"connection":"personal","method":"GET","key":"library/originals/x","size":1}));
        assert!(c.authorize_browser(&r, 300).is_err());
        r = request(json!({"connection":"personal","method":"GET","key":"library/originals/x","version":"a+/="}));
        assert!(c.authorize_browser(&r, 300).unwrap().url.contains("versionId=a%2B%2F%3D"));
    }
    #[test]
    fn signer_append_paths_require_canonical_content_addressing() {
        let c = connection();
        let digest = super::super::super::sha256(b"x").0;
        for (key, size) in [
            (format!("sync-v2/lib/manifest/{digest}.json"), 64 << 10),
            (format!("sync-v2/lib/checkpoints/author/{digest}.json"), 64 << 20),
            (format!("sync-v2/lib/ops/author/00000000000000000001-{digest}.json"), 2 << 20),
            (format!("sync-v2/lib/receipts/{digest}.json"), 1024),
        ] {
            let mut r = request(
                json!({"connection":"personal","method":"PUT","operation":"putImmutable","key":format!("library/{key}"),"size":size,"contentSha256":digest}),
            );
            assert!(c.authorize_browser(&r, 300).is_ok());
            r.size = Some(size + 1);
            assert!(c.authorize_browser(&r, 300).is_err());
            r.size = Some(size);
            r.condition = Some("*".into());
            assert!(c.authorize_browser(&r, 300).is_err());
        }
        assert_eq!(checksum(&super::super::super::sha256(b"").0).unwrap(), "47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=");
    }

    #[test]
    fn b2_catalog_grants_are_bounded_and_never_grant_heads_descriptors_or_media() {
        let mut c = connection();
        c.endpoint = Some("https://s3.us-east-005.backblazeb2.com".into());
        c.region = "us-east-005".into();
        let digest = super::super::super::sha256(b"metadata").0;
        for (key, max) in [
            (format!("catalog/changes/{digest}.json"), 2 << 20),
            (format!("catalog/checkpoints/{digest}.json"), 64 << 20),
            (format!("catalog/epochs/2/changes/{digest}.json"), 2 << 20),
            (format!("catalog/epochs/17/checkpoints/{digest}.json"), 64 << 20),
            (format!("catalog/legacy/{digest}.json"), crate::cloud_records::MAX_INTAKE),
        ] {
            let mut r = request(
                json!({"connection":"personal","method":"PUT","operation":"putImmutable","key":format!("library/{key}"),"size":max,"contentSha256":digest}),
            );
            let grant = c.authorize_browser(&r, 300).unwrap();
            assert_eq!(grant.headers["x-amz-checksum-sha256"], checksum(&digest).unwrap());
            assert!(!grant.headers.contains_key("if-match") && !grant.headers.contains_key("if-none-match"));
            r.size = Some(max + 1);
            assert!(c.authorize_browser(&r, 300).is_err());
            r.size = Some(max);
            r.condition = Some("*".into());
            assert!(c.authorize_browser(&r, 300).is_err());
            r.condition = None;
            for (endpoint, region) in [(None, "us-east-1"), (Some("https://us1"), "us1"), (Some("https://minio.example.com"), "us-east-1")] {
                let mut other = c.clone();
                other.endpoint = endpoint.map(str::to_owned);
                other.region = region.into();
                assert!(other.authorize_browser(&r, 300).is_err(), "no unconditional catalog grant for {endpoint:?}");
            }
        }
        for key in [
            "catalog/head.json".into(),
            "catalog/b2-authority.json".into(),
            format!("catalog/authority/{digest}.json"),
            format!("catalog/epochs/2/legacy/{digest}.json"),
            format!("catalog/epochs/02/changes/{digest}.json"),
            format!("catalog/changes/{}.json", "b".repeat(64)),
            format!("originals/{digest}"),
            format!("sync-v2/lib/control/00000000000000000001-{digest}.json"),
        ] {
            let r = request(
                json!({"connection":"personal","method":"PUT","operation":"putImmutable","key":format!("library/{key}"),"size":1,"contentSha256":digest}),
            );
            assert!(c.authorize_browser(&r, 300).is_err(), "{key}");
        }
    }
}
