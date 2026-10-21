//! Bounded error-body inspection, shared by metadata and original transfers.
use super::*;
use crate::cloud_status::provider::{MAX_ERROR_BODY, b2_download_cap};

pub(super) fn provider_limit(response: &mut crate::cloud_traffic::Response, cancel: &AtomicBool) -> Result<()> {
    if let Some(error) = response.throttled() {
        return Err(error);
    }
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
    if b2_download_cap(response.status, &bytes) || native_cap(response.status, &bytes) {
        if native_cap(response.status, &bytes) {
            response.provider_cap_code("b2AccountCapExceeded");
        } else {
            response.provider_cap();
        }
        return Err(AssetError::ProviderLimit { status: response.status });
    }
    Ok(())
}

fn native_cap(status: u16, bytes: &[u8]) -> bool {
    #[derive(serde::Deserialize)]
    struct Refusal {
        status: u16,
        code: String,
    }
    status == 403
        && serde_json::from_slice::<Refusal>(bytes).is_ok_and(|body| {
            body.status == status && matches!(body.code.as_str(), "cap_exceeded" | "transaction_cap_exceeded" | "download_cap_exceeded")
        })
}
