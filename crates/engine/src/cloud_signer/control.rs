//! The signer owns native B2 credentials and the complete transition validator.
use super::*;
use crate::cloud_records::b2_control::{Action, HEAD_LIMIT, Reply, Request, Snapshot};
use crate::cloud_status::{CloudFailure, FailureKind, Recovery};
use std::sync::atomic::AtomicBool;
#[cfg(test)]
mod tests;

fn scope_error() -> CloudFailure {
    CloudFailure::new(FailureKind::Authentication, "browser destination does not match this personal cloud signer; no authority changed")
        .with_remedy(Recovery::EditConnection)
}
fn token(expected: &str) -> Result<(), CloudFailure> {
    if !expected.strip_prefix("b2:").and_then(|s| s.split_once(':')).is_some_and(|(revision, hash)| {
        revision.parse::<u64>().is_ok_and(|n| n.to_string() == revision)
            && hash.len() == 64
            && hash.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
    }) {
        return Err(CloudFailure::integrity("invalid Backblaze authority concurrency token; reread the authority"));
    }
    Ok(())
}
pub(super) fn validate(connection: &S3Connection, request: &Request) -> Result<(), CloudFailure> {
    connection.validate().map_err(CloudFailure::from)?;
    let base = match &connection.endpoint {
        Some(endpoint) => format!("{}/{}/", endpoint.trim_end_matches('/'), connection.bucket),
        None => format!("https://{}.s3.{}.amazonaws.com/", connection.bucket, connection.region),
    };
    if request.connection != connection.id
        || request.prefix != connection.prefix
        || request.prefix.is_empty()
        || request.bucket_root.len() > 4096
        || request.bucket_root != request.bucket_root.trim()
        || request.bucket_root.chars().any(char::is_control)
        || request.bucket_root.contains(['?', '#'])
    {
        return Err(scope_error());
    }
    let root = Url::parse(&request.bucket_root).map_err(|_| scope_error())?;
    let expected = Url::parse(&base).map_err(|_| scope_error())?;
    if root.tls != expected.tls || !root.host.eq_ignore_ascii_case(&expected.host) || root.port != expected.port || root.path != expected.path {
        return Err(scope_error());
    }
    if connection.endpoint.as_deref().and_then(crate::cloud_setup::b2_runtime_endpoint).is_none_or(|(_, region)| region != connection.region) {
        return Err(CloudFailure::new(FailureKind::Unsupported, "this personal signer destination does not support Backblaze library control")
            .with_remedy(Recovery::EditConnection));
    }
    let head = match &request.action {
        Action::Read {} => return Ok(()),
        Action::Fence { expected, library } => {
            token(expected)?;
            return if lightcraft_catalog::assets::identifier(library) {
                Ok(())
            } else {
                Err(CloudFailure::integrity("invalid legacy source library identity"))
            };
        }
        Action::Initialize { head } => head,
        Action::Write { expected, head } => {
            token(expected)?;
            head
        }
    };
    let bytes = serde_json::to_vec(head).map_err(CloudFailure::from)?;
    if bytes.len() > HEAD_LIMIT {
        return Err(CloudFailure::integrity("Backblaze authority head exceeds its supported size"));
    }
    // Match native reads: newer envelopes are an update requirement, not damaged
    // remote history. Deserialize alone deliberately does not certify a Head.
    let head = crate::cloud_records::Head::decode(&bytes)?;
    let library = head.library.as_deref().ok_or_else(|| CloudFailure::integrity("Backblaze authority head has no library identity"))?;
    head.validate_scope_typed(&connection.prefix, library)?;
    if head.control.as_ref().is_some_and(|c| c.state == crate::cloud_control::State::Deleted) {
        return Err(CloudFailure::new(FailureKind::Unsupported, "whole-library erasure is not granted by the personal browser signer"));
    }
    Ok(())
}
pub(crate) fn execute(connection: &S3Connection, request: &Request) -> Result<Reply, CloudFailure> {
    // Also validate here so no future non-HTTP caller can bypass destination binding.
    validate(connection, request)?;
    let cancel = AtomicBool::new(false);
    match &request.action {
        Action::Read {} => {
            let current = connection.b2_try_read_head(&cancel)?.map(|(head, token, _)| Snapshot { head: Box::new(head), token });
            Ok(Reply::Read { current })
        }
        Action::Initialize { head } => {
            connection.b2_initialize_head(head, &cancel)?;
            Ok(Reply::Updated {})
        }
        Action::Write { expected, head } => {
            connection.b2_write_head(expected, head, &cancel)?;
            Ok(Reply::Updated {})
        }
        Action::Fence { expected, library } => {
            crate::cloud_upgrade::fence_b2_authority(connection, expected, library, &cancel)?;
            Ok(Reply::Updated {})
        }
    }
}
pub(super) fn status(failure: &CloudFailure) -> u16 {
    match failure.kind {
        FailureKind::Authentication => 403,
        FailureKind::Contention => 409,
        FailureKind::Connectivity => 503,
        FailureKind::Cancelled => 408,
        FailureKind::Integrity | FailureKind::InvalidHistory | FailureKind::Unsupported | FailureKind::Capacity => 422,
        FailureKind::LocalResource => 500,
    }
}
