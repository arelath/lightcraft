//! Browser-owned authorization and transfer boundary. Catalog state never contains tokens
//! or signed URLs; only the scoped, device-local connection configuration is persisted.
use lightcraft_engine::catalog::assets::{Blob, identifier};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const MAX_BROWSER_BLOB: u64 = lightcraft_engine::input_limits::ORIGINAL_BYTES;
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
    /// Receipt archives retain their original append namespace even after a
    /// head upgrade. Validate each family before choosing its scoped grant.
    pub fn b2_immutable_kind(&self, key: &str, bytes: &[u8]) -> Result<ImmutableKind, lightcraft_engine::cloud_status::CloudFailure> {
        self.validate_key(key)?;
        if key.starts_with(&format!("{}/catalog/", self.prefix)) {
            lightcraft_engine::cloud_records::b2_catalog::validate(&self.prefix, key, bytes)?;
            Ok(ImmutableKind::Catalog)
        } else {
            lightcraft_engine::cloud_append::immutable_metadata(key, bytes)?;
            Ok(ImmutableKind::Append)
        }
    }
    /// Cached destination capability only; service permissions are checked by
    /// the explicit operation, never by a UI frame.
    pub fn supports_b2_authority(&self) -> bool {
        self.bucket_root.is_some() && lightcraft_engine::cloud_setup::b2_runtime_endpoint(&self.storage_origin).is_some()
    }
    /// Authorized URL credentials can rotate without changing the object namespace.
    pub fn same_destination(&self, other: &Self) -> bool {
        self.id == other.id && self.prefix == other.prefix && self.storage_origin == other.storage_origin && self.bucket_root == other.bucket_root
    }
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
#[derive(Debug, PartialEq, Eq)]
pub enum ImmutableKind {
    Catalog,
    Append,
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
        let mut names = std::collections::BTreeSet::new();
        if self.url.len() > 16384
            || self.headers.len() > 16
            || self.headers.iter().any(|(name, value)| {
                !names.insert(name.to_ascii_lowercase())
                    || !matches!(
                        name.to_ascii_lowercase().as_str(),
                        "content-type" | "if-match" | "if-none-match" | "x-amz-content-sha256" | "x-amz-checksum-sha256"
                    )
                    || value.len() > 2048
                    || value.chars().any(char::is_control)
            })
        {
            return Err("invalid authorized URL response".into());
        }
        Ok(())
    }
    pub fn validate_body_binding(&self, digest: &str, checksum: &str) -> Result<(), String> {
        self.validate_headers()?;
        let header = |name: &str| self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str());
        if header("x-amz-content-sha256") != Some(digest) && header("x-amz-checksum-sha256") != Some(checksum) {
            return Err("write authorization must bind the exact body SHA-256/checksum".into());
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
pub mod cache;
pub mod durability;
#[cfg(target_arch = "wasm32")]
mod history;
#[cfg(target_arch = "wasm32")]
pub mod host;
#[cfg(target_arch = "wasm32")]
mod legacy;
pub mod memory;
#[cfg(target_arch = "wasm32")]
mod metadata;
#[cfg(target_arch = "wasm32")]
pub mod protocol;
#[cfg(target_arch = "wasm32")]
mod transport;
#[cfg(target_arch = "wasm32")]
mod upgrade;
#[cfg(target_arch = "wasm32")]
mod verification;
#[cfg(target_arch = "wasm32")]
pub use transport::Provider;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn b2_upgrade_capability_uses_only_canonical_cached_destinations() {
        let connection = Connection {
            id: "personal".into(),
            prefix: "library".into(),
            authorize_endpoint: "https://signer.example/authorize".into(),
            storage_origin: "https://s3.us-east-005.backblazeb2.com".into(),
            bucket_root: Some("https://s3.us-east-005.backblazeb2.com/photos/".into()),
        };
        assert!(connection.supports_b2_authority());
        assert!(!Connection { bucket_root: None, ..connection.clone() }.supports_b2_authority());
        for origin in [
            "https://us1",
            "https://s3.us-east-005.backblazeb2.com.example",
            "http://s3.us-east-005.backblazeb2.com",
            "https://s3.us-east-005.backblazeb2.com/path",
        ] {
            assert!(!Connection { storage_origin: origin.into(), ..connection.clone() }.supports_b2_authority(), "{origin}");
        }
    }
    #[test]
    fn upgraded_b2_heads_keep_receipt_archives_in_the_append_grant() {
        use lightcraft_engine::{
            catalog::replication::{Change, SyncState},
            cloud_records::{Batch, object_key},
        };
        let connection = Connection {
            id: "personal".into(),
            prefix: "library".into(),
            authorize_endpoint: "https://signer.example/authorize".into(),
            storage_origin: "https://s3.us-east-005.backblazeb2.com".into(),
            bucket_root: Some("https://s3.us-east-005.backblazeb2.com/photos/".into()),
        };
        let mut state = SyncState::new("example-library".into(), "browser".into(), "personal".into()).unwrap();
        for rating in [1, 5] {
            state.enqueue(vec![Change { entity: "photo:one".into(), field: "rating".into(), base: None, value: serde_json::json!(rating) }]).unwrap();
        }
        let plan = state.archive_plan().unwrap();
        assert!(!plan.objects.is_empty(), "rebase compacts superseded receipts");
        for (hash, node) in &plan.objects {
            let bytes = serde_json::to_vec(node).unwrap();
            let key = lightcraft_engine::cloud_history::key(&connection.prefix, &state.library, hash).unwrap();
            assert!(lightcraft_engine::cloud_records::b2_catalog::validate(&connection.prefix, &key, &bytes).is_err());
            assert_eq!(connection.b2_immutable_kind(&key, &bytes).unwrap(), ImmutableKind::Append);
        }
        let bytes = serde_json::to_vec(&Batch { version: 3, parent: None, imports: vec![], mutations: vec![], checkpoint_marker: true }).unwrap();
        let key = object_key(&connection.prefix, 2, "changes", &lightcraft_engine::assets::sha256(&bytes).0);
        assert_eq!(connection.b2_immutable_kind(&key, &bytes).unwrap(), ImmutableKind::Catalog);
        assert!(connection.b2_immutable_kind("library/catalog/head.json", &bytes).is_err());
        assert!(connection.b2_immutable_kind("library/originals/arbitrary", &bytes).is_err());
    }
    #[test]
    fn all_writes_require_body_binding_and_case_duplicate_headers_are_rejected() {
        let mut auth = Authorized { url: "https://bucket.example/object?signed".into(), expires_at: 1, headers: BTreeMap::new() };
        assert!(auth.validate_body_binding("digest", "checksum").is_err());
        auth.headers.insert("x-amz-checksum-sha256".into(), "checksum".into());
        auth.validate_body_binding("digest", "checksum").unwrap();
        assert!(auth.validate_body_binding("digest", "different").is_err());
        auth.headers.insert("X-Amz-Checksum-Sha256".into(), "different".into());
        assert!(auth.validate_headers().is_err());
        auth.headers.clear();
        auth.headers.insert("x-amz-content-sha256".into(), "digest".into());
        auth.validate_body_binding("digest", "checksum").unwrap();
    }
    #[test]
    fn destination_identity_includes_optional_bucket_root_and_excludes_authorization() {
        let base = Connection {
            id: "personal".into(),
            prefix: "library".into(),
            storage_origin: "https://objects.example".into(),
            authorize_endpoint: "https://auth.example/authorize".into(),
            bucket_root: None,
        };
        let mut rotated = base.clone();
        rotated.authorize_endpoint = "https://new-auth.example/authorize".into();
        assert!(base.same_destination(&rotated));
        let mut root = base.clone();
        root.bucket_root = Some("https://objects.example/photos/".into());
        assert!(!base.same_destination(&root));
        assert!(!root.same_destination(&base));
        for changed in [
            Connection { prefix: "other".into(), ..root.clone() },
            Connection { storage_origin: "https://other.example".into(), ..root.clone() },
            Connection { bucket_root: Some("https://objects.example/other/".into()), ..root.clone() },
            Connection { id: "other".into(), ..root.clone() },
        ] {
            assert!(!root.same_destination(&changed));
        }
        let mut root_rotated = root.clone();
        root_rotated.authorize_endpoint = rotated.authorize_endpoint;
        assert!(root.same_destination(&root_rotated));
    }
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
