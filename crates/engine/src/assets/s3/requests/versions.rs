//! Explicit version inventory/deletion for whole-library erasure, never orphan retention.
use super::*;
use std::collections::BTreeSet;
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectVersion {
    pub key: String,
    pub version: String,
    pub size: u64,
    pub etag: Option<String>,
    pub latest: bool,
}
fn invalid() -> AssetError {
    AssetError::Invalid("invalid or out-of-scope object version listing; nothing erased".into())
}
type VersionPage = (Vec<ObjectVersion>, Option<(String, String)>);
fn parse(bytes: &[u8], prefix: &str) -> Result<VersionPage> {
    parse_page(bytes, prefix, false)
}
fn parse_page(bytes: &[u8], prefix: &str, multipart: bool) -> Result<VersionPage> {
    if bytes.len() > 4 << 20 {
        return Err(invalid());
    }
    let mut reader = quick_xml::NsReader::from_reader(bytes);
    reader.config_mut().check_end_names = true;
    let mut path = Vec::<String>::new();
    let mut text = String::new();
    let mut fields = BTreeMap::new();
    let mut item = BTreeMap::new();
    let mut versions = Vec::new();
    let mut root = false;
    for _ in 0..100_000 {
        let (ns, event) = reader.read_resolved_event().map_err(|_| invalid())?;
        match event {
            Event::Start(ref s) | Event::Empty(ref s) => {
                if !matches!(
                    ns,
                    ResolveResult::Unbound | ResolveResult::Bound(quick_xml::name::Namespace(b"http://s3.amazonaws.com/doc/2006-03-01/"))
                ) {
                    return Err(invalid());
                }
                let name = std::str::from_utf8(s.local_name().as_ref()).map_err(|_| invalid())?.to_owned();
                if path.len() >= 12 || name.len() > 80 {
                    return Err(invalid());
                }
                if path.is_empty() {
                    if root || name != if multipart { "ListMultipartUploadsResult" } else { "ListVersionsResult" } {
                        return Err(invalid());
                    }
                    root = true;
                }
                for a in s.attributes() {
                    a.map_err(|_| invalid())?;
                }
                if path.len() == 1 && matches!(name.as_str(), "Version" | "DeleteMarker" | "Upload") {
                    item.clear();
                }
                path.push(name);
                text.clear();
                if matches!(event, Event::Empty(_)) {
                    path.pop();
                }
            }
            Event::Text(t) => {
                let value = t.xml_content().map_err(|_| invalid())?;
                if text.len().saturating_add(value.len()) > 8192 {
                    return Err(invalid());
                }
                text.push_str(&value);
            }
            Event::GeneralRef(r) => {
                text.push_str(&quick_xml::escape::unescape(&format!("&{};", r.decode().map_err(|_| invalid())?)).map_err(|_| invalid())?);
                if text.len() > 8192 {
                    return Err(invalid());
                }
            }
            Event::End(e) => {
                let name = path.pop().ok_or_else(invalid)?;
                if e.local_name().as_ref() != name.as_bytes() {
                    return Err(invalid());
                }
                if path.len() == 2
                    && matches!(path.get(1).map(String::as_str), Some("Version" | "DeleteMarker" | "Upload"))
                    && item.insert(name.clone(), text.clone()).is_some()
                {
                    return Err(invalid());
                }
                if path.len() == 1 {
                    if matches!(name.as_str(), "Version" | "DeleteMarker" | "Upload") {
                        if multipart != (name == "Upload") {
                            return Err(invalid());
                        }
                        let key = item.remove("Key").ok_or_else(invalid)?;
                        let version = item.remove(if multipart { "UploadId" } else { "VersionId" }).ok_or_else(invalid)?;
                        let latest = if multipart {
                            false
                        } else {
                            match item.remove("IsLatest").as_deref() {
                                Some("true") => true,
                                Some("false") => false,
                                _ => return Err(invalid()),
                            }
                        };
                        if !key.starts_with(prefix)
                            || crate::assets::keys::in_library("", &key).is_err()
                            || version.is_empty()
                            || version.len() > 1024
                            || version.chars().any(char::is_control)
                        {
                            return Err(invalid());
                        }
                        let size = if name == "Version" { item.remove("Size").and_then(|s| s.parse::<u64>().ok()).ok_or_else(invalid)? } else { 0 };
                        let token = item.remove("ETag").map(|s| etag(&s)).transpose()?;
                        if name == "Version" && token.is_none() {
                            return Err(invalid());
                        }
                        versions.push(ObjectVersion { key, version, size, etag: token, latest });
                        if versions.len() > 1000 {
                            return Err(invalid());
                        }
                    } else if fields.insert(name, text.clone()).is_some() {
                        return Err(invalid());
                    }
                }
                text.clear();
            }
            Event::DocType(_) | Event::CData(_) => return Err(invalid()),
            Event::Eof => {
                if !root || !path.is_empty() || fields.contains_key("CommonPrefixes") || fields.get("Prefix").map(String::as_str) != Some(prefix) {
                    return Err(invalid());
                }
                let next = match fields.get("IsTruncated").map(String::as_str) {
                    Some("false") => None,
                    Some("true") => Some((
                        fields.remove("NextKeyMarker").filter(|s| s.starts_with(prefix)).ok_or_else(invalid)?,
                        fields
                            .remove(if multipart { "NextUploadIdMarker" } else { "NextVersionIdMarker" })
                            .filter(|s| !s.is_empty() && s.len() <= 1024 && !s.chars().any(char::is_control))
                            .ok_or_else(invalid)?,
                    )),
                    _ => return Err(invalid()),
                };
                return Ok((versions, next));
            }
            _ => {}
        }
    }
    Err(invalid())
}
impl S3Connection {
    /// Every historical version and delete marker under the exact nonempty storage folder.
    /// Unsupported listing or denied permission fails closed, with no current-only fallback.
    pub fn library_versions(&self, cancel: &AtomicBool) -> Result<Vec<ObjectVersion>> {
        self.validate()?;
        if self.prefix.is_empty() {
            return Err(AssetError::Invalid("library erasure requires a dedicated nonempty storage folder".into()));
        }
        let prefix = format!("{}/", self.prefix);
        let mut next: Option<(String, String)> = None;
        let mut markers = BTreeSet::new();
        let mut seen = BTreeSet::new();
        let mut all = Vec::new();
        for _ in 0..256 {
            let mut query = vec![("versions", ""), ("prefix", prefix.as_str()), ("max-keys", "1000")];
            if let Some((key, version)) = &next {
                query.push(("key-marker", key));
                query.push(("version-id-marker", version));
            }
            let response = self.request(
                Request { method: Method::Get, key: "", query: &query, headers: &[], payload: &sha256(b"").0, body: None },
                cancel,
                &mut |_| {},
            )?;
            let reply = read_reply(response, 4 << 20, cancel)?;
            let (entries, following) = parse(&reply.bytes, &prefix)?;
            for entry in entries {
                if !seen.insert((entry.key.clone(), entry.version.clone())) || all.len() >= 100_000 {
                    return Err(invalid());
                }
                all.push(entry);
            }
            let Some(following) = following else {
                let mut latest = BTreeSet::new();
                let keys: BTreeSet<_> = all.iter().map(|v| v.key.as_str()).collect();
                if all.iter().any(|v| v.latest && !latest.insert(v.key.as_str())) || latest != keys {
                    return Err(invalid());
                }
                all.sort();
                return Ok(all);
            };
            if !markers.insert(following.clone()) {
                return Err(invalid());
            }
            next = Some(following);
        }
        Err(invalid())
    }
    pub fn library_multipart_uploads(&self, cancel: &AtomicBool) -> Result<Vec<(String, String)>> {
        if self.prefix.is_empty() {
            return Err(invalid());
        }
        let prefix = format!("{}/", self.prefix);
        let mut next: Option<(String, String)> = None;
        let mut seen = BTreeSet::new();
        let mut all = BTreeSet::new();
        for _ in 0..256 {
            let mut query = vec![("uploads", ""), ("prefix", prefix.as_str()), ("max-uploads", "1000")];
            if let Some((key, id)) = &next {
                query.push(("key-marker", key));
                query.push(("upload-id-marker", id));
            }
            let response = self.request(
                Request { method: Method::Get, key: "", query: &query, headers: &[], payload: &sha256(b"").0, body: None },
                cancel,
                &mut |_| {},
            )?;
            let reply = read_reply(response, 4 << 20, cancel)?;
            let (entries, following) = parse_page(&reply.bytes, &prefix, true)?;
            for v in entries {
                if !all.insert((v.key, v.version)) || all.len() > 100_000 {
                    return Err(invalid());
                }
            }
            let Some(following) = following else {
                return Ok(all.into_iter().collect());
            };
            if !seen.insert(following.clone()) {
                return Err(invalid());
            }
            next = Some(following);
        }
        Err(invalid())
    }
    pub fn erase_multipart_upload(&self, key: &str, id: &str, cancel: &AtomicBool) -> Result<()> {
        if self.prefix.is_empty() || !key.starts_with(&format!("{}/", self.prefix)) {
            return Err(invalid());
        }
        // This descriptor supplies only the operation address, not content evidence.
        // Reuse abort + ListParts verification, including late in-flight part handling.
        let blob = Blob { id: sha256(b""), size: 1, remote: RemoteLocation { connection: self.id.clone(), key: key.into(), version: None } };
        self.abort_multipart(&blob, id, cancel)
    }
    pub fn erase_version(&self, version: &ObjectVersion, cancel: &AtomicBool) -> Result<()> {
        if self.prefix.is_empty() || !version.key.starts_with(&format!("{}/", self.prefix)) {
            return Err(invalid());
        }
        identifier(&version.version)?;
        if version.version == "null" && version.etag.is_none() {
            return Err(invalid());
        }
        // Immutable version IDs select exact bytes. The mutable null version additionally
        // needs its reviewed ETag, so a concurrent replacement cannot be erased by accident.
        let headers = if version.version == "null" {
            version.etag.as_ref().map(|s| condition_headers(PutCondition::Matches(s))).transpose()?.unwrap_or_default()
        } else {
            Vec::new()
        };
        let mut response = self.request(
            Request {
                method: Method::Delete,
                key: &version.key,
                query: &[("versionId", &version.version)],
                headers: &headers,
                payload: &sha256(b"").0,
                body: None,
            },
            cancel,
            &mut |_| {},
        )?;
        status(&mut response, cancel)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn versions_include_hidden_history_and_markers_and_reject_scope_and_bad_xml() {
        let xml=b"<ListVersionsResult><Prefix>p/</Prefix><IsTruncated>false</IsTruncated><Version><Key>p/a</Key><VersionId>old</VersionId><IsLatest>false</IsLatest><Size>42</Size><ETag>&quot;old&quot;</ETag></Version><DeleteMarker><Key>p/a</Key><VersionId>gone</VersionId><IsLatest>true</IsLatest></DeleteMarker></ListVersionsResult>";
        let (v, next) = parse(xml, "p/").unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].size, 42);
        assert!(v[1].etag.is_none());
        assert!(next.is_none());
        assert!(parse(xml, "other/").is_err());
        assert!(parse(b"<ListVersionsResult><Prefix>p/</Prefix><IsTruncated>true</IsTruncated></ListVersionsResult>", "p/").is_err());
        assert!(parse(b"<!DOCTYPE a><ListVersionsResult/>", "p/").is_err());
    }
}
