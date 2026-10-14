//! Browser-owned authorization and transfer boundary. Catalog state never contains tokens
//! or signed URLs; only the scoped, device-local connection configuration is persisted.
use lightcraft_engine::catalog::assets::{Blob, identifier};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const MAX_BROWSER_BLOB: u64 = 512 << 20;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Connection {
    pub id: String,
    pub prefix: String,
    pub authorize_endpoint: String,
    pub storage_origin: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bucket_root: Option<String>,
}
impl Connection {
    pub fn validate(&self) -> Result<(), String> {
        if !identifier(&self.id)
            || self.prefix.is_empty()
            || self.prefix.len() > 1024
            || self.prefix.starts_with('/')
            || self.prefix.ends_with('/')
            || self.prefix.split('/').any(|part| part.is_empty() || matches!(part, "." | ".."))
            || self.prefix.chars().any(char::is_control)
            || self.prefix.contains('\\')
        {
            return Err("invalid browser storage connection or library prefix".into());
        }
        for url in [&self.authorize_endpoint, &self.storage_origin] {
            if url.len() > 4096
                || url.chars().any(char::is_control)
                || !(url.starts_with("https://") || url.starts_with("http://127.0.0.1:") || url.starts_with("http://localhost:"))
            {
                return Err("browser cloud URLs require HTTPS (loopback HTTP is allowed for tests)".into());
            }
        }
        Ok(())
    }
    pub fn validate_key(&self, key: &str) -> Result<(), String> {
        self.validate()?;
        if key.len() > 2048
            || !key.strip_prefix(&self.prefix).is_some_and(|tail| tail.starts_with('/') && tail.len() > 1)
            || key.split('/').any(|part| part.is_empty() || matches!(part, "." | ".."))
            || key.chars().any(char::is_control)
            || key.contains('\\')
        {
            return Err("cloud object is outside the configured library prefix".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Authorized {
    pub url: String,
    pub expires_at: u64,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}
impl Authorized {
    pub fn validate_headers(&self) -> Result<(), String> {
        if self.url.len() > 16384
            || self.headers.len() > 16
            || self.headers.iter().any(|(name, value)| {
                !matches!(
                    name.to_ascii_lowercase().as_str(),
                    "content-type" | "if-match" | "if-none-match" | "x-amz-content-sha256" | "x-amz-checksum-sha256"
                ) || value.len() > 2048
                    || value.chars().any(char::is_control)
            })
        {
            return Err("invalid authorized URL response".into());
        }
        Ok(())
    }
}
pub fn cache_key(blob: &Blob) -> String {
    format!("cloud/blobs/{}", blob.id.0)
}
pub fn verify_blob(blob: &Blob, bytes: &[u8]) -> Result<(), String> {
    if blob.size > MAX_BROWSER_BLOB {
        return Err("this blob exceeds the browser's 512 MiB transfer limit; use the desktop app".into());
    }
    if bytes.len() as u64 != blob.size || lightcraft_engine::assets::sha256(bytes) != blob.id {
        return Err("cloud blob failed SHA-256/length verification".into());
    }
    Ok(())
}

#[cfg(target_arch = "wasm32")]
mod append;
pub mod durability;
#[cfg(target_arch = "wasm32")]
mod history;
#[cfg(target_arch = "wasm32")]
pub mod host;
pub mod memory;
#[cfg(target_arch = "wasm32")]
pub mod protocol;
#[cfg(target_arch = "wasm32")]
mod transport;
#[cfg(target_arch = "wasm32")]
pub use transport::Provider;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prefix_scope_and_authorized_headers_are_bounded() {
        let c = Connection {
            id: "personal".into(),
            prefix: "library".into(),
            authorize_endpoint: "https://signer.example/authorize".into(),
            storage_origin: "https://bucket.example".into(),
            bucket_root: None,
        };
        c.validate_key("library/catalog/head.json").unwrap();
        for key in ["library-other/key", "library/../other", "library//key", "library/", "library/x\n"] {
            assert!(c.validate_key(key).is_err(), "{key}");
        }
        let auth = Authorized {
            url: "https://bucket.example/signed".into(),
            expires_at: 1,
            headers: BTreeMap::from([("Authorization".into(), "never accepted".into())]),
        };
        assert!(auth.validate_headers().is_err());
    }
    #[test]
    fn blobs_require_length_and_sha256_before_cache_publication() {
        let blob = Blob {
            id: lightcraft_engine::assets::sha256(b"verified"),
            size: 8,
            remote: lightcraft_engine::catalog::assets::RemoteLocation {
                connection: "personal".into(),
                key: "library/original".into(),
                version: None,
            },
        };
        verify_blob(&blob, b"verified").unwrap();
        assert!(verify_blob(&blob, b"corrupt!").is_err());
        assert!(verify_blob(&blob, b"verifie").is_err());
        assert_eq!(cache_key(&blob), format!("cloud/blobs/{}", blob.id.0));
    }
}
