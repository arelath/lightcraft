//! Product admission limits, distinct from the streamed cloud blob wire bound.
use serde_json::{Value, json};

pub const ORIGINAL_BYTES: u64 = 512 << 20;
pub const SHARED_PIXELS: u64 = 128_000_000;
pub const MAX_EDGE: u64 = 65_535;
pub const DECODE_PIXELS: u64 = if cfg!(target_arch = "wasm32") { 32_000_000 } else { SHARED_PIXELS };

pub fn bytes(size: u64) -> Result<(), String> {
    if size == 0 || size > ORIGINAL_BYTES {
        return Err(format!(
            "input is {size} bytes; the original input/decode limit is {} bytes (512 MiB). Keep the original and use a smaller lossless derivative",
            ORIGINAL_BYTES
        ));
    }
    Ok(())
}
pub fn dimensions(width: u64, height: u64, limit: u64) -> Result<(), String> {
    if width == 0 || height == 0 || width > MAX_EDGE || height > MAX_EDGE || width.saturating_mul(height) > limit {
        let recovery = if limit < SHARED_PIXELS {
            "Use a published smart preview for browser editing, or desktop for originals within its 128 MP limit"
        } else {
            "Keep the original and use a smaller lossless derivative"
        };
        return Err(format!("input is {width} x {height} pixels; this decoder permits {limit} pixels and {MAX_EDGE} per edge. {recovery}"));
    }
    Ok(())
}
pub fn shared_dimensions(width: u32, height: u32) -> bool {
    dimensions(u64::from(width), u64::from(height), SHARED_PIXELS).is_ok()
}
pub fn prefers_preview(width: u32, height: u32, bytes: u64) -> bool {
    u64::from(width).saturating_mul(u64::from(height)) > DECODE_PIXELS || bytes > ORIGINAL_BYTES
}
pub fn view() -> Value {
    json!({"originalBytes":ORIGINAL_BYTES,"decodePixels":DECODE_PIXELS,"sharedPhotoPixels":SHARED_PIXELS,
        "dimension":MAX_EDGE,"remoteBlobBytes":lightcraft_catalog::assets::MAX_BLOB_BYTES,
        "platform":if cfg!(target_arch="wasm32") {"browser"} else {"native"},
        "rawPixels":"full sensor before crop; vendor header probes may unpack sensor samples",
        "policy":"Byte, pixel and format limits all apply. Streamed blob capacity is not decoder capacity. Browser oversized managed photos require a smart preview; full original rendering requires desktop."})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn large_hasselblad_geometry_and_actionable_boundaries() {
        dimensions(11904, 8842, SHARED_PIXELS).unwrap();
        assert!(shared_dimensions(11656, 8742));
        assert!(dimensions(11904, 8842, 32_000_000).unwrap_err().contains("smart preview"));
        assert!(dimensions(u64::MAX, 2, SHARED_PIXELS).is_err());
        assert!(dimensions(1, 0, SHARED_PIXELS).is_err());
        assert!(dimensions(MAX_EDGE + 1, 1, SHARED_PIXELS).is_err());
        bytes(ORIGINAL_BYTES).unwrap();
        assert!(bytes(ORIGINAL_BYTES + 1).unwrap_err().contains("512 MiB"));
        assert!(bytes(0).is_err());
    }
}
