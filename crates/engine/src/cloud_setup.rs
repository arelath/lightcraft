//! Guided cloud storage setup: provider presets and a read-only connection check that also
//! finds the libraries already stored under the chosen folder. Keys typed into setup sign only
//! the check until they are saved to a profile (see [`crate::assets::s3::save_profile`]).

#[cfg(not(target_arch = "wasm32"))]
use crate::assets::{AssetError, s3::S3Connection};
use lightcraft_catalog::replication::SyncProtocol;
use serde::{Deserialize, Serialize};
#[cfg(not(target_arch = "wasm32"))]
use std::sync::atomic::AtomicBool;

/// Where the library is stored. It decides the endpoint and the protocol of a new library.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Provider {
    #[default]
    Backblaze,
    Aws,
    Other,
}

impl Provider {
    /// The protocol a new library uses here. B2 has no conditional writes and other
    /// S3-compatible services vary, so only AWS S3 publishes through a conditional head.
    pub fn new_library_protocol(self) -> SyncProtocol {
        match self {
            Self::Aws => SyncProtocol::ConditionalHead,
            Self::Backblaze | Self::Other => SyncProtocol::AppendLog,
        }
    }

    /// The provider a saved connection was set up for.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn of(connection: &S3Connection) -> Self {
        match connection.endpoint.as_deref() {
            None => Self::Aws,
            Some(endpoint) if b2_endpoint(endpoint).is_some() => Self::Backblaze,
            Some(_) => Self::Other,
        }
    }
}

/// The endpoint and region of a Backblaze B2 bucket from its "Endpoint" as B2 shows it
/// (`s3.us-west-004.backblazeb2.com`), as a URL, or from the bare region (`us-west-004`).
pub fn b2_endpoint(typed: &str) -> Option<(String, String)> {
    let host = typed.trim().trim_start_matches("https://").trim_end_matches('/').to_ascii_lowercase();
    let region = match host.strip_prefix("s3.").and_then(|h| h.strip_suffix(".backblazeb2.com")) {
        Some(region) => region.to_owned(),
        None if !host.contains('.') => host,
        None => return None,
    };
    let valid = (3..=64).contains(&region.len())
        && region.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && region.bytes().any(|b| b.is_ascii_digit());
    valid.then(|| (format!("https://s3.{region}.backblazeb2.com"), region))
}

/// The AWS credentials profile cloud setup saves a connection's keys under.
pub fn profile_for(connection: &str) -> String {
    format!("lightcraft-{connection}")
}

/// Run before the wizard saves typed credentials. Rejecting a destination edit
/// must leave both the connection and its existing credentials profile untouched.
#[cfg(not(target_arch = "wasm32"))]
pub fn validate_destination(session: &mut crate::Session, params: &serde_json::Value) -> Result<(), String> {
    let connection: S3Connection = serde_json::from_value(params.clone()).map_err(|_| "invalid storage connection settings")?;
    let configured = session.library.as_mut().ok_or("open a persistent library first")?.cloud_connections().map_err(|e| e.to_string())?;
    connection.validate_replacement(&session.catalog, &configured).map_err(|e| e.to_string())
}

/// A library found under the connection's folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FoundLibrary {
    pub id: String,
    pub protocol: SyncProtocol,
}

/// What the connection check found.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Probe {
    pub libraries: Vec<FoundLibrary>,
    /// The folder holds a conditional-head library (one per folder), so a new conditional-head
    /// library needs another folder.
    pub head_taken: bool,
}

/// Check that `connection` reaches its bucket with its keys and may list it, and find the
/// libraries stored under its folder. Read-only: it lists and reads, and never writes. The
/// error is a sentence saying what to fix.
#[cfg(not(target_arch = "wasm32"))]
pub fn probe(connection: &S3Connection, cancel: &AtomicBool) -> Result<Probe, String> {
    connection.validate().map_err(|e| e.to_string())?;
    let normalized = connection.clone().canonicalized();
    let connection = &normalized;
    let prefix = connection.prefix.trim_end_matches('/');
    let root = if prefix.is_empty() { "sync-v2/".to_owned() } else { format!("{prefix}/sync-v2/") };
    let request = crate::cloud_append::listing::ListRequest { prefix: root.clone(), delimiter: true, continuation: None, start_after: None };
    let page = connection.list_objects_v2(&request, cancel).map_err(|e| explain(connection, e))?;
    let libraries = page
        .prefixes
        .iter()
        .filter_map(|p| p.strip_prefix(&root)?.strip_suffix('/'))
        .filter(|id| lightcraft_catalog::assets::identifier(id))
        .map(|id| FoundLibrary { id: id.to_owned(), protocol: SyncProtocol::AppendLog })
        .collect();
    let mut probe = Probe { libraries, head_taken: false };
    match crate::cloud_publication::read_head(connection, cancel) {
        Ok(Some((head, _))) => {
            probe.head_taken = true;
            if let Some(id) = head.library {
                probe.libraries.insert(0, FoundLibrary { id, protocol: SyncProtocol::ConditionalHead });
            }
        }
        Ok(None) => {}
        Err(e) => return Err(explain(connection, e)),
    }
    Ok(probe)
}

/// What to fix when the check fails.
#[cfg(not(target_arch = "wasm32"))]
fn explain(connection: &S3Connection, error: AssetError) -> String {
    match error {
        AssetError::Unauthorized | AssetError::Http(401 | 403) => format!(
            "The storage refused these keys. Check the key ID and secret, and that the key may read, write and list the bucket “{}”.",
            connection.bucket
        ),
        AssetError::Missing => {
            format!("No bucket named “{}” was found. Check the bucket name, and the region or endpoint it was created in.", connection.bucket)
        }
        AssetError::Network(message) => format!("Couldn't reach the storage: {message}"),
        AssetError::Unconfigured(message) => format!("No keys to connect with: {message}"),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn b2_endpoints_are_read_as_b2_shows_them() {
        let want = Some(("https://s3.us-west-004.backblazeb2.com".to_owned(), "us-west-004".to_owned()));
        assert_eq!(b2_endpoint("s3.us-west-004.backblazeb2.com"), want);
        assert_eq!(b2_endpoint(" https://S3.us-west-004.backblazeb2.com/ "), want);
        assert_eq!(b2_endpoint("us-west-004"), want);
        assert_eq!(b2_endpoint("s3.amazonaws.com"), None);
        assert_eq!(b2_endpoint("photos"), None);
        assert_eq!(b2_endpoint(""), None);
        assert_eq!(b2_endpoint("us west"), None);
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn providers_pick_the_protocol_and_are_recognised_again() {
        assert_eq!(Provider::Aws.new_library_protocol(), SyncProtocol::ConditionalHead);
        assert_eq!(Provider::Backblaze.new_library_protocol(), SyncProtocol::AppendLog);
        assert_eq!(Provider::Other.new_library_protocol(), SyncProtocol::AppendLog);
        let mut connection: S3Connection = serde_json::from_value(serde_json::json!({"id":"c","bucket":"photos","region":"us-east-1"})).unwrap();
        assert_eq!(Provider::of(&connection), Provider::Aws);
        connection.endpoint = Some("https://s3.eu-central-003.backblazeb2.com".into());
        assert_eq!(Provider::of(&connection), Provider::Backblaze);
        connection.endpoint = Some("https://minio.example.com".into());
        assert_eq!(Provider::of(&connection), Provider::Other);
    }
}
