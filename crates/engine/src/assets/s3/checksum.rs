//! Qualified AWS SHA-256 upload evidence. ETags and user metadata never qualify.
use super::requests::{MAX_REPLY, PART_BYTES, Request, condition_headers, etag, identifier, parse_xml, read_reply};
use super::{PutCondition, S3Connection};
use crate::assets::{AssetError, Result, sha256};
use crate::cloud_metadata::{MediaVerification, immutable_version};
use lightcraft_catalog::assets::{Blob, BlobId};
use lightcraft_fetch::http::{Method, RequestBody};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Cursor, Read};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum MultipartChecksum {
    #[default]
    None,
    Sha256Requested,
    Sha256Confirmed,
}
impl MultipartChecksum {
    pub(crate) fn is_none(&self) -> bool {
        *self == Self::None
    }
}
pub(crate) struct PartRequest<'a> {
    pub upload_id: &'a str,
    pub number: u16,
    pub checksum: MultipartChecksum,
}
pub(crate) struct ChecksumPart {
    pub number: u16,
    pub etag: String,
    pub hash: BlobId,
    pub size: u64,
    pub checksum: Option<String>,
}
pub(crate) struct MultipartCompletion<'a> {
    pub upload_id: &'a str,
    pub parts: &'a [ChecksumPart],
    /// SHA-256 of the exact source chunks, including resumed/skipped parts.
    pub source_hash: &'a BlobId,
    pub checksum: MultipartChecksum,
    pub immutable: bool,
}
pub(crate) struct UploadEvidence {
    destination: String,
    key: String,
    hash: BlobId,
    size: u64,
    version: String,
    method: MediaVerification,
}
impl UploadEvidence {
    fn new(c: &S3Connection, blob: &Blob, version: Option<String>, method: MediaVerification) -> Option<Self> {
        let version = version.filter(|version| immutable_version(version))?;
        Some(Self {
            destination: crate::cloud_traffic::destination(c),
            key: blob.remote.key.clone(),
            hash: blob.id.clone(),
            size: blob.size,
            version,
            method,
        })
    }
    /// The upload proves one version's bytes; independently confirm that version
    /// is still applicable before reusing it for publication or recovery.
    pub(crate) fn certify(self, c: &S3Connection, blob: &Blob, cancel: &AtomicBool) -> Result<bool> {
        if cancel.load(Ordering::Relaxed) {
            return Err(AssetError::Cancelled);
        }
        if !c.aws_sha256()
            || self.destination != crate::cloud_traffic::destination(c)
            || self.key != blob.remote.key
            || self.hash != blob.id
            || self.size != blob.size
            || blob.remote.version.is_some()
        {
            return Err(AssetError::Integrity);
        }
        let _purpose = crate::cloud_traffic::Scope::purpose(crate::cloud_traffic::Purpose::UploadVerification);
        match c.stat_blob(blob, cancel) {
            Ok(Some(stat)) if stat.version.as_deref() == Some(self.version.as_str()) && stat.size == self.size => {
                c.remember_media_receipt(blob, Some(&self.version), self.method, cancel)?;
                Ok(true)
            }
            Ok(_) | Err(AssetError::Unsupported(_) | AssetError::Invalid(_) | AssetError::Http(405)) => Ok(false),
            Err(error) => Err(error),
        }
    }
}
fn invalid() -> AssetError {
    AssetError::Invalid("invalid SHA-256 upload evidence".into())
}
fn digest(hex: &str) -> Result<[u8; 32]> {
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')) {
        return Err(invalid());
    }
    let mut bytes = [0; 32];
    for (index, pair) in hex.as_bytes().chunks_exact(2).enumerate() {
        let high = pair.first().copied().and_then(|b| char::from(b).to_digit(16)).ok_or_else(invalid)?;
        let low = pair.get(1).copied().and_then(|b| char::from(b).to_digit(16)).ok_or_else(invalid)?;
        *bytes.get_mut(index).ok_or_else(invalid)? = u8::try_from(high * 16 + low).map_err(|_| invalid())?;
    }
    Ok(bytes)
}
fn base64(bytes: &[u8; 32]) -> Result<String> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(44);
    for chunk in bytes.chunks(3) {
        let a = chunk.first().copied().unwrap_or(0);
        let b = chunk.get(1).copied().unwrap_or(0);
        let c = chunk.get(2).copied().unwrap_or(0);
        for (position, index) in [a >> 2, ((a & 3) << 4) | (b >> 4), ((b & 15) << 2) | (c >> 6), c & 63].into_iter().enumerate() {
            if position > chunk.len() {
                out.push('=');
            } else {
                out.push(char::from(*ALPHABET.get(usize::from(index)).ok_or_else(invalid)?));
            }
        }
    }
    Ok(out)
}
pub(super) fn base64_digest(hex: &str) -> Result<String> {
    base64(&digest(hex)?)
}
pub(crate) fn part_checksum(hash: &BlobId) -> Result<String> {
    base64_digest(&hash.0)
}
fn composite(parts: &[ChecksumPart]) -> Result<String> {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update(digest(&part.hash.0)?);
    }
    Ok(format!("{}-{}", base64(&hash.finalize().into())?, parts.len()))
}
struct Payload<'a> {
    source: &'a mut dyn Read,
    hash: Sha256,
    count: u64,
    size: u64,
}
impl Read for Payload<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let count = self.source.read(bytes)?;
        let data = bytes.get(..count).ok_or_else(|| std::io::Error::other("invalid upload source length"))?;
        self.count = self
            .count
            .checked_add(u64::try_from(count).map_err(|_| std::io::Error::other("upload length overflow"))?)
            .filter(|count| *count <= self.size)
            .ok_or_else(|| std::io::Error::other("upload source exceeds its expected size"))?;
        self.hash.update(data);
        Ok(count)
    }
}
impl S3Connection {
    fn aws_sha256(&self) -> bool {
        self.endpoint.is_none()
    }
    pub(crate) fn put_blob_checksum(
        &self,
        blob: &Blob,
        source: &mut dyn Read,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(u64),
        immutable: bool,
    ) -> Result<Option<UploadEvidence>> {
        if !self.aws_sha256() {
            return self.put_blob_observed(blob, source, cancel, progress, immutable).map(|_| None);
        }
        self.validate_blob(blob)?;
        if blob.remote.version.is_some() {
            return Err(invalid());
        }
        if immutable {
            crate::cloud_append::immutable_blob(blob)?;
        }
        let expected = part_checksum(&blob.id)?;
        let mut headers = if immutable { Vec::new() } else { condition_headers(PutCondition::Absent)? };
        headers.push(("x-amz-checksum-sha256", expected.clone()));
        let mut payload = Payload { source, hash: Sha256::new(), count: 0, size: blob.size };
        let response = self.request(
            Request {
                method: Method::Put,
                key: &blob.remote.key,
                query: &[],
                headers: &headers,
                payload: &blob.id.0,
                body: Some(RequestBody { reader: &mut payload, length: blob.size }),
            },
            cancel,
            progress,
        )?;
        let reply = read_reply(response, MAX_REPLY, cancel)?;
        if payload.count != blob.size || format!("{:x}", payload.hash.finalize()) != blob.id.0 {
            return Err(AssetError::Integrity);
        }
        if reply.checksum.as_ref().is_some_and(|actual| actual != &expected)
            || reply.checksum_type.as_deref().is_some_and(|kind| kind != "FULL_OBJECT")
        {
            return Err(AssetError::Integrity);
        }
        if reply.checksum.as_ref() != Some(&expected) {
            return Ok(None);
        }
        Ok(UploadEvidence::new(self, blob, reply.version, MediaVerification::AwsPutSha256))
    }
    pub(crate) fn start_multipart_checksum(&self, blob: &Blob, cancel: &AtomicBool) -> Result<(String, MultipartChecksum)> {
        if !self.aws_sha256() {
            return self.start_multipart(blob, cancel).map(|id| (id, MultipartChecksum::None));
        }
        self.validate_blob(blob)?;
        if blob.remote.version.is_some() {
            return Err(invalid());
        }
        let headers = [("x-amz-checksum-algorithm", "SHA256".into()), ("x-amz-checksum-type", "COMPOSITE".into())];
        let response = self.request(
            Request {
                method: Method::Post,
                key: &blob.remote.key,
                query: &[("uploads", "")],
                headers: &headers,
                payload: &sha256(b"").0,
                body: Some(RequestBody { reader: &mut Cursor::new([]), length: 0 }),
            },
            cancel,
            &mut |_| {},
        )?;
        let reply = read_reply(response, MAX_REPLY, cancel)?;
        let xml = parse_xml(&reply.bytes, "InitiateMultipartUploadResult")?;
        let id = xml.get("UploadId").ok_or_else(invalid)?;
        identifier(id)?;
        if reply.checksum_algorithm.as_deref().is_some_and(|algorithm| algorithm != "SHA256")
            || reply.checksum_type.as_deref().is_some_and(|kind| kind != "COMPOSITE")
        {
            return Err(AssetError::Integrity);
        }
        let mode = if reply.checksum_algorithm.as_deref() == Some("SHA256") && reply.checksum_type.as_deref() == Some("COMPOSITE") {
            MultipartChecksum::Sha256Confirmed
        } else {
            MultipartChecksum::Sha256Requested
        };
        Ok((id.clone(), mode))
    }
    pub(crate) fn upload_part_checksum(
        &self,
        blob: &Blob,
        part: PartRequest<'_>,
        bytes: &[u8],
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(u64),
    ) -> Result<(String, Option<String>)> {
        if part.checksum.is_none() {
            return self.upload_part(blob, part.upload_id, part.number, bytes, cancel, progress).map(|etag| (etag, None));
        }
        self.validate_blob(blob)?;
        identifier(part.upload_id)?;
        if !self.aws_sha256() || blob.remote.version.is_some() || !(1..=10_000).contains(&part.number) || bytes.is_empty() || bytes.len() > PART_BYTES
        {
            return Err(invalid());
        }
        let hash = sha256(bytes);
        let expected = part_checksum(&hash)?;
        let number = part.number.to_string();
        let headers = [("x-amz-checksum-sha256", expected.clone())];
        let response = self.request(
            Request {
                method: Method::Put,
                key: &blob.remote.key,
                query: &[("partNumber", &number), ("uploadId", part.upload_id)],
                headers: &headers,
                payload: &hash.0,
                body: Some(RequestBody { reader: &mut Cursor::new(bytes), length: bytes.len() as u64 }),
            },
            cancel,
            progress,
        )?;
        let reply = read_reply(response, MAX_REPLY, cancel)?;
        if reply.checksum.as_ref().is_some_and(|actual| actual != &expected) {
            return Err(AssetError::Integrity);
        }
        Ok((reply.etag.ok_or_else(invalid)?, reply.checksum))
    }
    pub(crate) fn complete_multipart_checksum(
        &self,
        blob: &Blob,
        completion: MultipartCompletion<'_>,
        cancel: &AtomicBool,
    ) -> Result<Option<UploadEvidence>> {
        self.validate_blob(blob)?;
        identifier(completion.upload_id)?;
        if blob.remote.version.is_some() || completion.source_hash != &blob.id {
            return Err(AssetError::Integrity);
        }
        if completion.immutable {
            crate::cloud_append::immutable_blob(blob)?;
        }
        let parts = completion.parts;
        if parts.is_empty() || parts.len() > 512 {
            return Err(invalid());
        }
        let mut total = 0u64;
        for (index, part) in parts.iter().enumerate() {
            if usize::from(part.number) != index.saturating_add(1) || part.size == 0 || part.size > PART_BYTES as u64 {
                return Err(invalid());
            }
            etag(&part.etag)?;
            let expected = part_checksum(&part.hash)?;
            if part.checksum.as_ref().is_some_and(|actual| actual != &expected) {
                return Err(AssetError::Integrity);
            }
            total = total.checked_add(part.size).ok_or_else(invalid)?;
        }
        if total != blob.size {
            return Err(AssetError::Integrity);
        }
        if completion.checksum.is_none() {
            let parts: Vec<_> = parts.iter().map(|part| (part.number, part.etag.clone())).collect();
            return self.complete_multipart_observed(blob, completion.upload_id, &parts, cancel, completion.immutable).map(|_| None);
        }
        if !self.aws_sha256() {
            return Err(invalid());
        }
        let expected = composite(parts)?;
        let mut xml = String::from("<CompleteMultipartUpload>");
        for part in parts {
            xml.push_str(&format!(
                "<Part><PartNumber>{}</PartNumber><ETag>{}</ETag><ChecksumSHA256>{}</ChecksumSHA256></Part>",
                part.number,
                quick_xml::escape::escape(&part.etag),
                part_checksum(&part.hash)?
            ));
        }
        xml.push_str("</CompleteMultipartUpload>");
        let mut headers = if completion.immutable { Vec::new() } else { condition_headers(PutCondition::Absent)? };
        headers.push(("content-type", "application/xml".into()));
        headers.push(("x-amz-checksum-type", "COMPOSITE".into()));
        let response = self.request(
            Request {
                method: Method::Post,
                key: &blob.remote.key,
                query: &[("uploadId", completion.upload_id)],
                headers: &headers,
                payload: &sha256(xml.as_bytes()).0,
                body: Some(RequestBody { reader: &mut Cursor::new(xml.as_bytes()), length: xml.len() as u64 }),
            },
            cancel,
            &mut |_| {},
        )?;
        let reply = read_reply(response, MAX_REPLY, cancel)?;
        let fields = parse_xml(&reply.bytes, "CompleteMultipartUploadResult")?;
        etag(fields.get("ETag").ok_or_else(invalid)?)?;
        if fields.get("ChecksumSHA256").is_some_and(|actual| actual != &expected)
            || fields.get("ChecksumType").is_some_and(|kind| kind != "COMPOSITE")
            || fields.get("Bucket").is_some_and(|bucket| bucket != &self.bucket)
            || fields.get("Key").is_some_and(|key| key != &blob.remote.key)
        {
            return Err(AssetError::Integrity);
        }
        if completion.checksum != MultipartChecksum::Sha256Confirmed
            || parts.iter().any(|part| part.checksum.is_none())
            || fields.get("ChecksumSHA256") != Some(&expected)
            || fields.get("ChecksumType").map(String::as_str) != Some("COMPOSITE")
            || fields.get("Bucket") != Some(&self.bucket)
            || fields.get("Key") != Some(&blob.remote.key)
        {
            return Ok(None);
        }
        Ok(UploadEvidence::new(self, blob, reply.version, MediaVerification::AwsMultipartSha256))
    }
}

#[cfg(test)]
mod tests;
