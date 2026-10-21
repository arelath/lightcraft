//! Bounded signed object writes and multipart primitives. Callers own durable receipts,
//! remote byte verification and publication ordering; an ETag is never a checksum.

use super::{S3Connection, Signing, encode, http_error, sign_request};
use crate::assets::{AssetError, Result, sha256};
use crate::cloud_traffic::{self, Response, Target};
use lightcraft_catalog::assets::{Blob, RemoteLocation};
use lightcraft_fetch::http::{Limits, Method, RequestBody, Url};
use quick_xml::events::Event;
use quick_xml::name::ResolveResult;
use std::collections::BTreeMap;
use std::io::{Cursor, Read};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

mod versions;
pub use versions::ObjectVersion;

pub(super) const MAX_REPLY: usize = 2 << 20;
const MAX_CHECKPOINT: usize = 64 << 20;
pub(super) const PART_BYTES: usize = 8 << 20;

#[derive(Debug)]
pub struct ObjectReply {
    pub bytes: Vec<u8>,
    pub etag: Option<String>,
    pub version: Option<String>,
    pub checksum: Option<String>,
    pub checksum_type: Option<String>,
    pub checksum_algorithm: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectStat {
    pub size: u64,
    pub etag: String,
    pub modified: String,
    pub version: Option<String>,
}

#[derive(Clone, Copy)]
pub enum PutCondition<'a> {
    Absent,
    Matches(&'a str),
}

pub(super) struct Request<'a> {
    pub(super) method: Method,
    pub(super) key: &'a str,
    pub(super) query: &'a [(&'a str, &'a str)],
    pub(super) headers: &'a [(&'static str, String)],
    pub(super) payload: &'a str,
    pub(super) body: Option<RequestBody<'a>>,
}

pub(super) fn identifier(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > 1024 || id.chars().any(char::is_control) {
        return Err(AssetError::Invalid("invalid multipart upload identity".into()));
    }
    Ok(())
}

pub(super) fn etag(value: &str) -> Result<String> {
    if value.is_empty() || value == "*" || value.len() > 512 || value.chars().any(char::is_control) {
        return Err(AssetError::Unsupported("storage returned an invalid concurrency token".into()));
    }
    Ok(value.into())
}

pub(super) fn condition_headers(condition: PutCondition<'_>) -> Result<Vec<(&'static str, String)>> {
    Ok(vec![match condition {
        PutCondition::Absent => ("if-none-match", "*".into()),
        PutCondition::Matches(value) => ("if-match", etag(value)?),
    }])
}

fn status(response: &mut Response, cancel: &AtomicBool) -> Result<()> {
    super::error::provider_limit(response, cancel)?;
    match response.status {
        200..=299 => Ok(()),
        code @ (401 | 403) => Err(AssetError::Http(code)),
        301 | 307 | 308 => Err(AssetError::Unsupported("the bucket answers from another region or endpoint; check the region and endpoint".into())),
        404 => Err(AssetError::Missing),
        409 | 412 => Err(AssetError::Conflict),
        501 => Err(AssetError::Unsupported("endpoint rejected this S3 operation (HTTP 501); safe catalog publication requires If-None-Match and If-Match support; staged data is retained".into())),
        code => Err(AssetError::Http(code)),
    }
}

pub(super) fn read_reply(mut response: Response, max: usize, cancel: &AtomicBool) -> Result<ObjectReply> {
    status(&mut response, cancel)?;
    if max > MAX_CHECKPOINT || response.content_length().is_some_and(|size| size > max as u64) {
        return Err(AssetError::Invalid("S3 response exceeds its safety limit".into()));
    }
    let etag = response.header("etag").map(etag).transpose()?;
    let version = response.header("x-amz-version-id").map(str::to_owned);
    let checksum = response.header("x-amz-checksum-sha256").map(str::to_owned);
    let checksum_type = response.header("x-amz-checksum-type").map(str::to_owned);
    let checksum_algorithm = response.header("x-amz-checksum-algorithm").map(str::to_owned);
    let limits = Limits { connect: Duration::from_secs(15), stall: Duration::from_secs(30), cancel };
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 64 << 10];
    loop {
        let n = response.read(&mut buffer, &limits).map_err(http_error)?;
        if n == 0 {
            break;
        }
        if bytes.len().saturating_add(n) > max {
            return Err(AssetError::Invalid("S3 response exceeds its safety limit".into()));
        }
        bytes.try_reserve(n).map_err(|_| AssetError::Disk("not enough memory for the S3 response".into()))?;
        bytes.extend_from_slice(buffer.get(..n).ok_or(AssetError::Integrity)?);
    }
    Ok(ObjectReply { bytes, etag, version, checksum, checksum_type, checksum_algorithm })
}

impl S3Connection {
    /// Retention uses complete object identity, never a listing's age alone.
    pub fn stat_object(&self, key: &str, cancel: &AtomicBool) -> Result<Option<ObjectStat>> {
        self.stat_version(key, None, cancel)
    }
    pub(crate) fn stat_blob(&self, blob: &Blob, cancel: &AtomicBool) -> Result<Option<ObjectStat>> {
        self.validate_blob(blob)?;
        self.stat_version(&blob.remote.key, blob.remote.version.as_deref(), cancel)
    }
    fn stat_version(&self, key: &str, version: Option<&str>, cancel: &AtomicBool) -> Result<Option<ObjectStat>> {
        let query: Vec<_> = version.map(|version| ("versionId", version)).into_iter().collect();
        let mut response = self.request(
            Request { method: Method::Head, key, query: &query, headers: &[], payload: &sha256(b"").0, body: None },
            cancel,
            &mut |_| {},
        )?;
        if response.status == 404 {
            return Ok(None);
        }
        status(&mut response, cancel)?;
        Ok(Some(ObjectStat {
            // HEAD has no response body; the HTTP client's body length is None.
            size: response
                .header("content-length")
                .and_then(|value| value.parse().ok())
                .ok_or_else(|| AssetError::Invalid("retention needs object Content-Length; nothing deleted".into()))?,
            etag: etag(response.header("etag").ok_or_else(|| AssetError::Invalid("retention needs an object ETag; nothing deleted".into()))?)?,
            modified: response
                .header("last-modified")
                .filter(|v| v.len() <= 128)
                .ok_or_else(|| AssetError::Invalid("retention needs Last-Modified; nothing deleted".into()))?
                .into(),
            version: response.header("x-amz-version-id").map(str::to_owned),
        }))
    }
    /// No unconditional fallback. Versioned objects are outside orphan reclamation.
    pub fn delete_unversioned(&self, key: &str, expected: &ObjectStat, cancel: &AtomicBool) -> Result<()> {
        if expected.version.is_some() || self.stat_object(key, cancel)?.as_ref() != Some(expected) {
            return Err(AssetError::Conflict);
        }
        let headers = condition_headers(PutCondition::Matches(&expected.etag))?;
        let mut response = self.request(
            Request { method: Method::Delete, key, query: &[], headers: &headers, payload: &sha256(b"").0, body: None },
            cancel,
            &mut |_| {},
        )?;
        status(&mut response, cancel)?;
        if self.stat_object(key, cancel)?.is_some() {
            return Err(AssetError::Integrity);
        }
        Ok(())
    }
    fn request_url(&self, key: &str, query: &[(&str, &str)]) -> Result<Url> {
        // Reuse the download boundary's connection/key/prefix validation.
        let mut url = if key.is_empty() {
            self.validate()?;
            let base = match &self.endpoint {
                Some(e) => format!("{}/{}/", e.trim_end_matches('/'), self.bucket),
                None => format!("https://{}.s3.{}.amazonaws.com/", self.bucket, self.region),
            };
            #[cfg(test)]
            let base = self.metadata.endpoint.as_ref().map_or(base, |endpoint| format!("{endpoint}/{}/", self.bucket));
            Url::parse(&base).map_err(|_| AssetError::Invalid("invalid bucket root".into()))?
        } else {
            let blob = Blob { id: sha256(b""), size: 1, remote: RemoteLocation { connection: self.id.clone(), key: key.into(), version: None } };
            self.url(&blob)?
        };
        if query.len() > 8
            || query.iter().any(|(key, value)| key.len() > 128 || value.len() > 8192 || key.chars().chain(value.chars()).any(char::is_control))
        {
            return Err(AssetError::Invalid("invalid S3 operation query".into()));
        }
        let mut encoded = query.iter().map(|(key, value)| (encode(key, false), encode(value, false))).collect::<Vec<_>>();
        encoded.sort_unstable();
        if !encoded.is_empty() {
            url.path.push('?');
            url.path.push_str(&encoded.iter().map(|(key, value)| format!("{key}={value}")).collect::<Vec<_>>().join("&"));
        }
        Ok(url)
    }

    pub(super) fn request(&self, request: Request<'_>, cancel: &AtomicBool, progress: &mut dyn FnMut(u64)) -> Result<Response> {
        let url = self.request_url(request.key, request.query)?;
        let credentials = self.credentials()?;
        let now = time::OffsetDateTime::now_utc();
        let timestamp =
            format!("{:04}{:02}{:02}T{:02}{:02}{:02}Z", now.year(), u8::from(now.month()), now.day(), now.hour(), now.minute(), now.second());
        let headers = sign_request(
            &url,
            &self.region,
            &credentials,
            &timestamp,
            Signing { method: request.method, payload: request.payload, extra: request.headers },
        )?;
        let limits = Limits { connect: Duration::from_secs(15), stall: Duration::from_secs(30), cancel };
        cloud_traffic::request(
            Target {
                destination: &cloud_traffic::destination(self),
                resource: &url.path,
                transport: "s3",
                kind: if request.method == Method::Get
                    && request.query.iter().any(|(k, _)| matches!(*k, "list-type" | "versions" | "uploads" | "uploadId"))
                {
                    "listing"
                } else {
                    "object"
                },
                method: request.method,
            },
            &url,
            &headers,
            request.body,
            &limits,
            progress,
        )
        .map_err(http_error)
    }

    /// Read only small protocol objects. Missing objects are distinct from access failures.
    pub fn get_small(&self, key: &str, max: usize, cancel: &AtomicBool) -> Result<Option<ObjectReply>> {
        if max > MAX_CHECKPOINT {
            return Err(AssetError::Invalid("metadata limit exceeds 64 MiB".into()));
        }
        if let Some(budget) = &self.metadata.budget {
            budget.admit(max.max(16 << 10))?;
        }
        let response =
            self.request(Request { method: Method::Get, key, query: &[], headers: &[], payload: &sha256(b"").0, body: None }, cancel, &mut |_| {})?;
        if response.status == 404 {
            return Ok(None);
        }
        let reply = read_reply(response, max, cancel)?;
        if let Some(budget) = &self.metadata.budget {
            budget.read(max.max(16 << 10), reply.bytes.len());
        }
        Ok(Some(reply))
    }

    pub fn put_small(&self, key: &str, bytes: &[u8], condition: PutCondition<'_>, cancel: &AtomicBool) -> Result<String> {
        if bytes.len() > MAX_CHECKPOINT {
            return Err(AssetError::Invalid("metadata object exceeds 64 MiB".into()));
        }
        let headers = condition_headers(condition)?;
        let response = self.request(
            Request {
                method: Method::Put,
                key,
                query: &[],
                headers: &headers,
                payload: &sha256(bytes).0,
                body: Some(RequestBody { reader: &mut Cursor::new(bytes), length: bytes.len() as u64 }),
            },
            cancel,
            &mut |_| {},
        )?;
        let reply = read_reply(response, MAX_REPLY, cancel)?;
        reply.etag.ok_or_else(|| AssetError::Unsupported("storage did not return an ETag; conditional publication is unavailable".into()))
    }

    /// Only content-addressed append records may be written without preconditions.
    /// Success always includes a full readback; response loss retries the same bytes/key.
    pub fn put_immutable_verified(&self, key: &str, bytes: &[u8], cancel: &AtomicBool) -> Result<()> {
        crate::cloud_append::immutable_metadata(key, bytes)?;
        self.put_verified_metadata(key, bytes, cancel)
    }
    /// The owning metadata adapter must validate its exact content-addressed namespace.
    /// This is intentionally unavailable outside the native S3 implementation.
    pub(super) fn put_verified_metadata(&self, key: &str, bytes: &[u8], cancel: &AtomicBool) -> Result<()> {
        if bytes.is_empty() || bytes.len() > MAX_CHECKPOINT {
            return Err(AssetError::Invalid("immutable metadata exceeds its bounded length".into()));
        }
        if let Some(existing) = self.get_small(key, MAX_CHECKPOINT, cancel)? {
            return if existing.bytes == bytes { Ok(()) } else { Err(AssetError::Integrity) };
        }
        for attempt in 0..2 {
            let result = self
                .request(
                    Request {
                        method: Method::Put,
                        key,
                        query: &[],
                        headers: &[],
                        payload: &sha256(bytes).0,
                        body: Some(RequestBody { reader: &mut Cursor::new(bytes), length: bytes.len() as u64 }),
                    },
                    cancel,
                    &mut |_| {},
                )
                .and_then(|response| read_reply(response, MAX_REPLY, cancel));
            let result = match result {
                Err(error) if !error.uncertain_write() => return Err(error),
                result => result,
            };
            match self.get_small(key, MAX_CHECKPOINT, cancel) {
                Ok(Some(reply)) => return if reply.bytes == bytes { Ok(()) } else { Err(AssetError::Integrity) },
                Ok(None) if attempt == 0 && (result.is_ok() || result.as_ref().is_err_and(AssetError::uncertain_write)) => continue,
                Err(e) => return Err(e),
                _ => {
                    return result
                        .map(|_| ())
                        .and_then(|_| Err(AssetError::Network("immutable publication is not yet visible; pending work retained".into())));
                }
            }
        }
        Err(AssetError::Network("immutable publication could not be verified".into()))
    }
    pub fn list_objects_v2(
        &self,
        list: &crate::cloud_append::listing::ListRequest,
        cancel: &AtomicBool,
    ) -> Result<crate::cloud_append::listing::Page> {
        list.validate(&self.prefix)?;
        if let Some(budget) = &self.metadata.budget {
            budget.admit(4 << 20)?;
        }
        let mut query = vec![("list-type", "2"), ("max-keys", "1000"), ("prefix", list.prefix.as_str())];
        if list.delimiter {
            query.push(("delimiter", "/"));
        }
        if let Some(token) = &list.continuation {
            query.push(("continuation-token", token));
        }
        if let Some(after) = &list.start_after {
            query.push(("start-after", after));
        }
        let response = self.request(
            Request { method: Method::Get, key: "", query: &query, headers: &[], payload: &sha256(b"").0, body: None },
            cancel,
            &mut |_| {},
        )?;
        let reply = read_reply(response, 4 << 20, cancel)?;
        if let Some(budget) = &self.metadata.budget {
            budget.read(4 << 20, reply.bytes.len());
        }
        crate::cloud_append::listing::parse(&reply.bytes, list)
    }

    pub fn put_blob(&self, blob: &Blob, reader: &mut dyn Read, cancel: &AtomicBool, progress: &mut dyn FnMut(u64)) -> Result<()> {
        self.put_blob_observed(blob, reader, cancel, progress, false).map(|_| ())
    }
    pub fn put_blob_immutable(&self, blob: &Blob, reader: &mut dyn Read, cancel: &AtomicBool, progress: &mut dyn FnMut(u64)) -> Result<()> {
        crate::cloud_append::immutable_blob(blob)?;
        self.put_blob_observed(blob, reader, cancel, progress, true).map(|_| ())
    }
    pub fn put_blob_observed(
        &self,
        blob: &Blob,
        reader: &mut dyn Read,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(u64),
        immutable: bool,
    ) -> Result<Option<String>> {
        blob.validate().map_err(AssetError::Invalid)?;
        if immutable {
            crate::cloud_append::immutable_blob(blob)?;
        }
        if blob.remote.version.is_some() {
            return Err(AssetError::Invalid("uploads cannot target a historical object version".into()));
        }
        let headers = if immutable { Vec::new() } else { condition_headers(PutCondition::Absent)? };
        let response = self.request(
            Request {
                method: Method::Put,
                key: &blob.remote.key,
                query: &[],
                headers: &headers,
                payload: &blob.id.0,
                body: Some(RequestBody { reader, length: blob.size }),
            },
            cancel,
            progress,
        )?;
        Ok(read_reply(response, MAX_REPLY, cancel)?.version)
    }

    pub fn start_multipart(&self, blob: &Blob, cancel: &AtomicBool) -> Result<String> {
        blob.validate().map_err(AssetError::Invalid)?;
        if blob.remote.version.is_some() {
            return Err(AssetError::Invalid("multipart uploads cannot target a historical version".into()));
        }
        let response = self.request(
            Request {
                method: Method::Post,
                key: &blob.remote.key,
                query: &[("uploads", "")],
                headers: &[],
                payload: &sha256(b"").0,
                body: Some(RequestBody { reader: &mut Cursor::new([]), length: 0 }),
            },
            cancel,
            &mut |_| {},
        )?;
        let reply = read_reply(response, MAX_REPLY, cancel)?;
        let xml = parse_xml(&reply.bytes, "InitiateMultipartUploadResult")?;
        let id = xml.get("UploadId").ok_or_else(|| AssetError::Invalid("multipart response has no upload identity".into()))?;
        identifier(id)?;
        Ok(id.clone())
    }

    pub fn upload_part(
        &self,
        blob: &Blob,
        upload_id: &str,
        number: u16,
        bytes: &[u8],
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(u64),
    ) -> Result<String> {
        blob.validate().map_err(AssetError::Invalid)?;
        if blob.remote.version.is_some() {
            return Err(AssetError::Invalid("multipart uploads cannot target a historical version".into()));
        }
        identifier(upload_id)?;
        if !(1..=10_000).contains(&number) || bytes.is_empty() || bytes.len() > PART_BYTES {
            return Err(AssetError::Invalid("invalid multipart part number or size".into()));
        }
        let number = number.to_string();
        let response = self.request(
            Request {
                method: Method::Put,
                key: &blob.remote.key,
                query: &[("partNumber", &number), ("uploadId", upload_id)],
                headers: &[],
                payload: &sha256(bytes).0,
                body: Some(RequestBody { reader: &mut Cursor::new(bytes), length: bytes.len() as u64 }),
            },
            cancel,
            progress,
        )?;
        let reply = read_reply(response, MAX_REPLY, cancel)?;
        reply.etag.ok_or_else(|| AssetError::Invalid("multipart response has no part ETag".into()))
    }

    pub fn complete_multipart(&self, blob: &Blob, upload_id: &str, parts: &[(u16, String)], cancel: &AtomicBool) -> Result<()> {
        self.complete_multipart_observed(blob, upload_id, parts, cancel, false).map(|_| ())
    }
    pub fn complete_multipart_immutable(&self, blob: &Blob, upload_id: &str, parts: &[(u16, String)], cancel: &AtomicBool) -> Result<()> {
        crate::cloud_append::immutable_blob(blob)?;
        self.complete_multipart_observed(blob, upload_id, parts, cancel, true).map(|_| ())
    }
    pub fn complete_multipart_observed(
        &self,
        blob: &Blob,
        upload_id: &str,
        parts: &[(u16, String)],
        cancel: &AtomicBool,
        immutable: bool,
    ) -> Result<Option<String>> {
        blob.validate().map_err(AssetError::Invalid)?;
        if immutable {
            crate::cloud_append::immutable_blob(blob)?;
        }
        if blob.remote.version.is_some() {
            return Err(AssetError::Invalid("multipart uploads cannot target a historical version".into()));
        }
        identifier(upload_id)?;
        if parts.is_empty() || parts.len() > 512 || parts.iter().enumerate().any(|(index, (number, _))| usize::from(*number) != index + 1) {
            return Err(AssetError::Invalid("multipart completion requires consecutive parts in ascending order".into()));
        }
        let mut xml = String::from("<CompleteMultipartUpload>");
        for (number, token) in parts {
            let token = etag(token)?;
            xml.push_str(&format!("<Part><PartNumber>{number}</PartNumber><ETag>{}</ETag></Part>", quick_xml::escape::escape(&token)));
        }
        xml.push_str("</CompleteMultipartUpload>");
        let mut headers = if immutable { Vec::new() } else { condition_headers(PutCondition::Absent)? };
        headers.push(("content-type", "application/xml".into()));
        let response = self.request(
            Request {
                method: Method::Post,
                key: &blob.remote.key,
                query: &[("uploadId", upload_id)],
                headers: &headers,
                payload: &sha256(xml.as_bytes()).0,
                body: Some(RequestBody { reader: &mut Cursor::new(xml.as_bytes()), length: xml.len() as u64 }),
            },
            cancel,
            &mut |_| {},
        )?;
        let reply = read_reply(response, MAX_REPLY, cancel)?;
        // S3 may return HTTP 200 before deciding that completion failed.
        let fields = parse_xml(&reply.bytes, "CompleteMultipartUploadResult")?;
        etag(fields.get("ETag").ok_or_else(|| AssetError::Invalid("completion response has no ETag".into()))?)?;
        Ok(reply.version)
    }

    /// Abort only a recorded upload identity. Cancellation alone retains resumable parts.
    pub fn abort_multipart(&self, blob: &Blob, upload_id: &str, cancel: &AtomicBool) -> Result<()> {
        blob.validate().map_err(AssetError::Invalid)?;
        if blob.remote.version.is_some() {
            return Err(AssetError::Invalid("multipart uploads cannot target a historical version".into()));
        }
        identifier(upload_id)?;
        for _ in 0..3 {
            let response = self.request(
                Request {
                    method: Method::Delete,
                    key: &blob.remote.key,
                    query: &[("uploadId", upload_id)],
                    headers: &[],
                    payload: &sha256(b"").0,
                    body: None,
                },
                cancel,
                &mut |_| {},
            )?;
            if response.status == 404 {
                return Ok(());
            }
            read_reply(response, MAX_REPLY, cancel)?;
            // In-flight part writes can outlive an abort acknowledgement. Confirm
            // that no parts remain before discarding our only resumable receipt.
            let response = self.request(
                Request {
                    method: Method::Get,
                    key: &blob.remote.key,
                    query: &[("max-parts", "1"), ("uploadId", upload_id)],
                    headers: &[],
                    payload: &sha256(b"").0,
                    body: None,
                },
                cancel,
                &mut |_| {},
            )?;
            if response.status == 404 {
                return Ok(());
            }
            let reply = read_reply(response, MAX_REPLY, cancel)?;
            let fields = parse_xml(&reply.bytes, "ListPartsResult")?;
            if fields.get("UploadId").map(String::as_str) != Some(upload_id)
                || fields.get("Key") != Some(&blob.remote.key)
                || fields.get("Bucket") != Some(&self.bucket)
                || !matches!(fields.get("IsTruncated").map(String::as_str), Some("true" | "false"))
            {
                return Err(AssetError::Invalid("invalid multipart abort confirmation; receipt retained".into()));
            }
            if !fields.contains_key("Part") && fields.get("IsTruncated").map(String::as_str) == Some("false") {
                return Ok(());
            }
        }
        Err(AssetError::Network("multipart parts remain after abort; receipt retained for another cleanup attempt".into()))
    }
}

pub(super) fn parse_xml(bytes: &[u8], expected: &str) -> Result<BTreeMap<String, String>> {
    fn invalid() -> AssetError {
        AssetError::Invalid("invalid or unsupported S3 XML response".into())
    }
    if bytes.len() > MAX_REPLY {
        return Err(invalid());
    }
    let mut reader = quick_xml::NsReader::from_reader(bytes);
    reader.config_mut().check_end_names = true;
    let mut buffer = Vec::new();
    let mut path: Vec<String> = Vec::new();
    let mut fields = BTreeMap::<String, String>::new();
    let mut root = None;
    let mut saw_error = false;
    for _ in 0..20_000 {
        let (namespace, event) = reader.read_resolved_event_into(&mut buffer).map_err(|_| invalid())?;
        match event {
            Event::Start(ref start) | Event::Empty(ref start) => {
                if !matches!(
                    namespace,
                    ResolveResult::Unbound | ResolveResult::Bound(quick_xml::name::Namespace(b"http://s3.amazonaws.com/doc/2006-03-01/"))
                ) {
                    return Err(invalid());
                }
                let name = std::str::from_utf8(start.local_name().as_ref()).map_err(|_| invalid())?.to_string();
                if name.len() > 80 || path.len() >= 16 {
                    return Err(invalid());
                }
                for attribute in start.attributes() {
                    attribute.map_err(|_| invalid())?;
                }
                if path.is_empty() {
                    if root.is_some() {
                        return Err(invalid());
                    }
                    root = Some(name.clone());
                }
                saw_error |= name == "Error";
                path.push(name);
                if path.len() == 2
                    && path.last().is_some_and(|name| {
                        matches!(
                            name.as_str(),
                            "UploadId" | "ETag" | "Code" | "Bucket" | "Key" | "IsTruncated" | "Part" | "ChecksumSHA256" | "ChecksumType"
                        )
                    })
                {
                    let name = path.last().ok_or_else(invalid)?.clone();
                    if fields.insert(name, String::new()).is_some() {
                        return Err(invalid());
                    }
                }
                if matches!(event, Event::Empty(_)) {
                    path.pop();
                }
            }
            Event::End(end) => {
                let expected = path.pop().ok_or_else(invalid)?;
                if expected != std::str::from_utf8(end.local_name().as_ref()).map_err(|_| invalid())? {
                    return Err(invalid());
                }
            }
            Event::Text(text) => {
                let text = text.xml_content().map_err(|_| invalid())?;
                if path.is_empty() && !text.trim().is_empty() {
                    return Err(invalid());
                }
                if path.len() == 2
                    && let Some(value) = path.last().and_then(|name| fields.get_mut(name))
                {
                    if value.len().saturating_add(text.len()) > 2048 {
                        return Err(invalid());
                    }
                    value.push_str(&text);
                }
            }
            Event::GeneralRef(reference) => {
                let encoded = format!("&{};", reference.decode().map_err(|_| invalid())?);
                let decoded = quick_xml::escape::unescape(&encoded).map_err(|_| invalid())?;
                if path.len() == 2
                    && let Some(value) = path.last().and_then(|name| fields.get_mut(name))
                {
                    if value.len().saturating_add(decoded.len()) > 2048 {
                        return Err(invalid());
                    }
                    value.push_str(&decoded);
                }
            }
            Event::DocType(_) | Event::CData(_) => return Err(invalid()),
            Event::Eof => {
                if !path.is_empty() || root.is_none() {
                    return Err(invalid());
                }
                if saw_error {
                    let code = fields
                        .get("Code")
                        .filter(|code| code.len() <= 64 && code.bytes().all(|b| b.is_ascii_alphanumeric()))
                        .map(String::as_str)
                        .unwrap_or("Error");
                    if matches!(code, "NotImplemented" | "NotSupported") {
                        return Err(AssetError::Unsupported(format!(
                            "multipart completion returned {code}; conditional writes are required; upload receipt and staged bytes are retained"
                        )));
                    }
                    let message = format!("S3 multipart completion returned {code}; upload receipt and staged bytes retained");
                    return Err(match code {
                        "InternalError" | "ServiceUnavailable" | "SlowDown" | "RequestTimeout" => AssetError::Network(message),
                        "AccessDenied" | "InvalidAccessKeyId" | "SignatureDoesNotMatch" | "ExpiredToken" => AssetError::Unauthorized,
                        "NoSuchUpload" => AssetError::Missing,
                        _ => AssetError::Invalid(message),
                    });
                }
                if root.as_deref() != Some(expected) {
                    return Err(invalid());
                }
                return Ok(fields);
            }
            _ => {}
        }
        buffer.clear();
    }
    Err(invalid())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_error_codes_have_explicit_retry_policy_without_parsing_messages() {
        use crate::cloud_status::{CloudFailure, FailureKind};
        for (code, kind, retryable) in [
            ("InternalError", FailureKind::Connectivity, true),
            ("SlowDown", FailureKind::Connectivity, true),
            ("AccessDenied", FailureKind::Authentication, false),
            ("ExpiredToken", FailureKind::Authentication, false),
            ("NoSuchUpload", FailureKind::InvalidHistory, false),
            ("InvalidPart", FailureKind::InvalidHistory, false),
        ] {
            let xml = format!("<Error><Code>{code}</Code><Message>timeout network retry</Message></Error>");
            let error = parse_xml(xml.as_bytes(), "CompleteMultipartUploadResult").unwrap_err();
            let failure = CloudFailure::from(&error);
            assert_eq!(failure.kind, kind, "{code}");
            assert_eq!(failure.retryable(), retryable, "{code}");
        }
    }

    #[test]
    fn multipart_xml_resolves_namespaces_and_entities_and_refuses_errors() {
        assert!(matches!(parse_xml(b"<Error><Code>NotImplemented</Code></Error>", "CompleteMultipartUploadResult"), Err(AssetError::Unsupported(_))));
        for xml in [
            "<InitiateMultipartUploadResult><UploadId>a&amp;b&#43;</UploadId></InitiateMultipartUploadResult>",
            "<s:InitiateMultipartUploadResult xmlns:s=\"http://s3.amazonaws.com/doc/2006-03-01/\"><s:UploadId>a&amp;b&#43;</s:UploadId></s:InitiateMultipartUploadResult>",
        ] {
            assert_eq!(parse_xml(xml.as_bytes(), "InitiateMultipartUploadResult").unwrap().get("UploadId").unwrap(), "a&b+");
        }
        for xml in [
            "<Error><Code>InternalError</Code></Error>",
            "<CompleteMultipartUploadResult><Error/></CompleteMultipartUploadResult>",
            "<CompleteMultipartUploadResult xmlns=\"https://wrong.invalid/\"/>",
            "<CompleteMultipartUploadResult/><CompleteMultipartUploadResult/>",
            "<CompleteMultipartUploadResult><ETag>x</ETag><ETag>y</ETag></CompleteMultipartUploadResult>",
            "<!DOCTYPE x [<!ENTITY token 'x'>]><CompleteMultipartUploadResult/>",
            "<CompleteMultipartUploadResult><ETag>x</CompleteMultipartUploadResult>",
        ] {
            assert!(parse_xml(xml.as_bytes(), "CompleteMultipartUploadResult").is_err(), "{xml}");
        }
        assert!(parse_xml(&vec![b' '; MAX_REPLY + 1], "CompleteMultipartUploadResult").is_err());
        let deep = format!("{}{}", "<x>".repeat(18), "</x>".repeat(18));
        assert!(parse_xml(deep.as_bytes(), "x").is_err());
    }
}
