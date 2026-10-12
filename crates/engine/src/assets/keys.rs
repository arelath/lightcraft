//! Versioned immutable object keys. Descriptor hashes select a rendition; the final SHA-256
//! identifies/verifies its exact bytes. These never replace the disposable local render keys.

use super::{AssetError, Result};
use lightcraft_catalog::assets::{Blob, BlobId};

pub const SMART_VERSION: u32 = 1;
pub const THUMB_VERSION: u32 = 1;
pub const MAX_THUMB_EDGE: u32 = 1024;

fn digest(id: &BlobId) -> Result<&str> {
    if id.0.len() == 64 && id.0.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        Ok(&id.0)
    } else {
        Err(AssetError::Invalid("object identity must be a lowercase SHA-256 digest".into()))
    }
}

pub fn original(source: &BlobId) -> Result<String> {
    Ok(format!("originals/{}", digest(source)?))
}

pub fn smart_preview(source: &BlobId, payload: &BlobId) -> Result<String> {
    Ok(format!("previews/{}/v{SMART_VERSION}/{}.lcsp", digest(source)?, digest(payload)?))
}

/// Always cropped, oriented, display-ready PNG/sRGB8; rendering parameters include the
/// pipeline/camera-look versions and any source header facts used by the producer.
pub fn thumbnail(source: &BlobId, recipe: u64, parameters: u128, edge: u32, payload: &BlobId) -> Result<String> {
    if !(1..=MAX_THUMB_EDGE).contains(&edge) {
        return Err(AssetError::Invalid("cloud thumbnail edge must be between 1 and 1024 pixels".into()));
    }
    Ok(format!("thumbs/{}/v{THUMB_VERSION}/{recipe:016x}/{parameters:032x}/{edge}/{}.png", digest(source)?, digest(payload)?))
}

pub fn in_library(prefix: &str, object: &str) -> Result<String> {
    let key = if prefix.is_empty() { object.to_owned() } else { format!("{prefix}/{object}") };
    if key.len() > 1024 || key.contains('\\') || key.chars().any(char::is_control) || key.split('/').any(|p| p.is_empty() || matches!(p, "." | ".."))
    {
        return Err(AssetError::Invalid("invalid library object prefix/key".into()));
    }
    Ok(key)
}

/// Refuse thumbnails for another source, recipe, render context or format version. The
/// catalog's Asset thumbnail is shared by virtual copies, whose edits may differ.
pub fn thumbnail_edge(blob: &Blob, source: &BlobId, recipe: u64, parameters: u128) -> Option<u32> {
    if blob.validate().is_err() || digest(source).is_err() {
        return None;
    }
    let parts: Vec<_> = blob.remote.key.rsplit('/').take(7).collect();
    if parts.len() != 7
        || parts.get(6).copied()? != "thumbs"
        || parts.get(5).copied()? != source.0
        || parts.get(4).copied()? != format!("v{THUMB_VERSION}")
        || parts.get(3).copied()? != format!("{recipe:016x}")
        || parts.get(2).copied()? != format!("{parameters:032x}")
        || parts.first().copied()?.strip_suffix(".png")? != blob.id.0
    {
        return None;
    }
    parts.get(1)?.parse::<u32>().ok().filter(|edge| (1..=MAX_THUMB_EDGE).contains(edge))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_catalog::assets::RemoteLocation;

    #[test]
    fn renditions_are_versioned_by_source_recipe_parameters_and_exact_bytes() {
        let source = BlobId("a".repeat(64));
        let payload = BlobId("b".repeat(64));
        let key = thumbnail(&source, 42, 7, 512, &payload).unwrap();
        let blob = Blob {
            id: payload.clone(),
            size: 32,
            remote: RemoteLocation { connection: "photos".into(), key: in_library("libraries/example", &key).unwrap(), version: None },
        };
        assert_eq!(thumbnail_edge(&blob, &source, 42, 7), Some(512));
        assert_eq!(thumbnail_edge(&blob, &source, 43, 7), None);
        assert_eq!(thumbnail_edge(&blob, &source, 42, 8), None);
        assert_eq!(thumbnail_edge(&blob, &BlobId("c".repeat(64)), 42, 7), None);
        let mut next = blob.clone();
        next.remote.key = next.remote.key.replace("/v1/", "/v2/");
        assert_eq!(thumbnail_edge(&next, &source, 42, 7), None);
        next = blob;
        next.id = BlobId("c".repeat(64));
        assert_eq!(thumbnail_edge(&next, &source, 42, 7), None);
        assert!(original(&source).unwrap().starts_with("originals/"));
        assert!(smart_preview(&source, &payload).unwrap().contains("/v1/"));
        assert_ne!(key, thumbnail(&source, 42, 7, 256, &payload).unwrap());
        assert_ne!(key, thumbnail(&source, 42, 7, 512, &BlobId("c".repeat(64))).unwrap());
    }

    #[test]
    fn hostile_keys_and_thumbnail_limits_are_rejected() {
        let source = BlobId("a".repeat(64));
        for prefix in ["../other", "/absolute", "empty//segment", "bad\\separator", "bad\nheader"] {
            assert!(in_library(prefix, "originals/a").is_err());
        }
        assert!(original(&BlobId("../../other".into())).is_err());
        assert!(thumbnail(&source, 0, 0, 0, &source).is_err());
        assert!(thumbnail(&source, 0, 0, 1025, &source).is_err());
    }
}
