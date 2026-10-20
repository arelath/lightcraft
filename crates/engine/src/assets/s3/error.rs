//! Bounded error-body inspection, shared by metadata and original transfers.
use super::*;
use crate::cloud_status::provider::{MAX_ERROR_BODY, b2_download_cap};

pub(super) fn provider_limit(response: &mut http::Response, cancel: &AtomicBool) -> Result<()> {
    if response.status != 403 || response.content_length().is_some_and(|n| n > MAX_ERROR_BODY as u64) {
        return Ok(());
    }
    let limits = Limits { connect: Duration::from_secs(15), stall: Duration::from_secs(30), cancel };
    let mut bytes = Vec::new();
    let mut buffer = [0; 2048];
    loop {
        let n = match response.read(&mut buffer, &limits) {
            Ok(n) => n,
            Err(http::HttpError::Cancelled) => return Err(AssetError::Cancelled),
            // Error-body inspection must not replace an established HTTP refusal.
            Err(_) => return Ok(()),
        };
        if n == 0 {
            break;
        }
        if bytes.len().saturating_add(n) > MAX_ERROR_BODY {
            return Ok(());
        }
        bytes.try_reserve(n).map_err(|_| AssetError::Disk("not enough memory for storage diagnostic".into()))?;
        bytes.extend_from_slice(buffer.get(..n).ok_or(AssetError::Integrity)?);
    }
    if b2_download_cap(response.status, &bytes) {
        return Err(AssetError::ProviderLimit { status: response.status });
    }
    Ok(())
}
