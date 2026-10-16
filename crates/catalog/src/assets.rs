//! Location-independent asset records. Credentials and cache paths never belong in the catalog.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AssetId(pub String);

/// SHA-256 of exact immutable bytes; separate from the legacy duplicate/cache hash.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BlobId(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoteLocation {
    pub connection: String,
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Blob {
    pub id: BlobId,
    pub size: u64,
    pub remote: RemoteLocation,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Asset {
    pub original: Blob,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smart_preview: Option<Blob>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<Blob>,
    /// Immutable supporting documents, currently XMP. Mutable editor files are device-local.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub associated: BTreeMap<String, Blob>,
}

pub const MAX_BLOB_BYTES: u64 = 4 << 30;

pub fn identifier(s: &str) -> bool {
    !s.is_empty() && s.len() <= 128 && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

impl Blob {
    pub fn validate(&self) -> Result<(), String> {
        if self.id.0.len() != 64 || !self.id.0.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return Err("blob identity must be a lowercase SHA-256 hex digest".into());
        }
        if self.size == 0 || self.size > MAX_BLOB_BYTES {
            return Err("blob size must be between 1 byte and 4 GiB".into());
        }
        if !identifier(&self.remote.connection) {
            return Err("invalid storage connection identity".into());
        }
        let key = &self.remote.key;
        if key.is_empty()
            || key.len() > 1024
            || key.starts_with('/')
            || key.contains('\\')
            || key.chars().any(char::is_control)
            || key.split('/').any(|p| p.is_empty() || p == "." || p == "..")
        {
            return Err("invalid remote object key".into());
        }
        if self.remote.version.as_ref().is_some_and(|v| v.is_empty() || v.len() > 1024 || v.chars().any(char::is_control)) {
            return Err("invalid remote object version".into());
        }
        Ok(())
    }
}

impl Asset {
    /// Connection identities belong to the device; every other manifest field is immutable.
    pub fn same_manifest(&self, other: &Self) -> bool {
        let mut left = self.clone();
        let mut right = other.clone();
        left.bind_connection("library");
        right.bind_connection("library");
        left == right
    }

    /// Bind every shared blob, including supporting documents, to a local connection.
    pub fn rebound(&self, connection: &str) -> Result<Self, String> {
        self.validate()?;
        let mut asset = self.clone();
        asset.bind_connection(connection);
        asset.validate()?;
        Ok(asset)
    }

    fn bind_connection(&mut self, connection: &str) {
        for blob in std::iter::once(&mut self.original)
            .chain(self.smart_preview.iter_mut())
            .chain(self.thumbnail.iter_mut())
            .chain(self.associated.values_mut())
        {
            blob.remote.connection = connection.into();
        }
    }

    pub fn blobs(&self) -> impl Iterator<Item = &Blob> {
        std::iter::once(&self.original).chain(self.smart_preview.iter()).chain(self.thumbnail.iter()).chain(self.associated.values())
    }
    pub fn validate(&self) -> Result<(), String> {
        self.original.validate()?;
        for blob in [&self.smart_preview, &self.thumbnail].into_iter().flatten() {
            blob.validate()?;
        }
        if self.associated.len() > 16 {
            return Err("too many associated asset documents".into());
        }
        for (kind, blob) in &self.associated {
            if kind != "xmp" || blob.size > 16 << 20 {
                return Err("associated documents currently support XMP up to 16 MiB".into());
            }
            blob.validate()?;
            if blob.remote.connection != self.original.remote.connection {
                return Err("associated documents must use the original's connection".into());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn blob() -> Blob {
        Blob {
            id: BlobId("a".repeat(64)),
            size: 12,
            remote: RemoteLocation { connection: "personal".into(), key: "library/objects/a".into(), version: None },
        }
    }

    #[test]
    fn hostile_references_are_rejected() {
        let good = blob();
        assert!(good.validate().is_ok());
        for key in ["../outside", "a/../../b", "/absolute", "a//b", "a\\b", "a\nheader", ""] {
            let mut bad = good.clone();
            bad.remote.key = key.into();
            assert!(bad.validate().is_err(), "{key}");
        }
        let mut bad = good;
        bad.id = BlobId("../escape".into());
        assert!(bad.validate().is_err());
        bad = blob();
        bad.size = u64::MAX;
        assert!(bad.validate().is_err());
    }

    #[test]
    fn complete_manifests_are_immutable_except_for_device_connection_bindings() {
        use crate::{Catalog, Journal, MemStore, Op};
        let asset = Asset {
            original: blob(),
            smart_preview: Some(Blob { remote: RemoteLocation { key: "preview".into(), ..blob().remote }, ..blob() }),
            thumbnail: Some(Blob { remote: RemoteLocation { key: "thumbnail".into(), ..blob().remote }, ..blob() }),
            associated: BTreeMap::from([("xmp".into(), Blob { remote: RemoteLocation { key: "sidecar".into(), ..blob().remote }, ..blob() })]),
        };
        let id = AssetId("manifest".into());
        let mut catalog = Catalog::new();
        let first = Op::SetAsset { id: id.clone(), asset: Some(Box::new(asset.clone())) };
        catalog.apply(first.clone()).unwrap();
        let mut rejected = Vec::new();
        // Every blob's bytes, length, key and optional version are part of the contract.
        for index in 0..4 {
            for field in 0..4 {
                let mut changed = asset.clone();
                let mut blobs = std::iter::once(&mut changed.original)
                    .chain(changed.smart_preview.iter_mut())
                    .chain(changed.thumbnail.iter_mut())
                    .chain(changed.associated.values_mut());
                let blob = blobs.nth(index).unwrap();
                match field {
                    0 => blob.id = BlobId("b".repeat(64)),
                    1 => blob.size += 1,
                    2 => blob.remote.key.push_str("-changed"),
                    _ => blob.remote.version = Some("new-version".into()),
                }
                rejected.push(changed);
            }
        }
        let mut changed = asset.clone();
        changed.smart_preview = None;
        rejected.push(changed);
        let mut changed = asset.clone();
        changed.thumbnail = None;
        rejected.push(changed);
        let mut changed = asset.clone();
        changed.associated.clear();
        rejected.push(changed);
        for changed in rejected {
            let before = serde_json::to_value(&catalog).unwrap();
            assert!(catalog.apply(Op::SetAsset { id: id.clone(), asset: Some(Box::new(changed.clone())) }).is_err());
            assert_eq!(serde_json::to_value(&catalog).unwrap(), before);
            // A new identity can describe the replacement without changing the old one.
            let mut replacement = catalog.clone();
            replacement.apply(Op::SetAsset { id: AssetId("replacement".into()), asset: Some(Box::new(changed)) }).unwrap();
            assert_eq!(replacement.asset(&id), Some(&asset));
        }
        // Addition is also a manifest change, even if the original is untouched.
        let mut original_only = Catalog::new();
        original_only
            .apply(Op::SetAsset {
                id: id.clone(),
                asset: Some(Box::new(Asset { original: blob(), smart_preview: None, thumbnail: None, associated: BTreeMap::new() })),
            })
            .unwrap();
        assert!(original_only.apply(first.clone()).is_err());
        let rebound = asset.rebound("second-device").unwrap();
        assert!(asset.same_manifest(&rebound));
        assert!(rebound.blobs().all(|b| b.remote.connection == "second-device"));
        assert!(asset.rebound("../invalid").is_err());
        let working = crate::managed::WorkingCopy {
            connection: "personal".into(),
            path: "Managed/working/photo.raw".into(),
            asset_id: id.clone(),
            ingested: asset.original.id.clone(),
        };
        let working_op = Op::SetWorkingCopy { id: crate::PhotoId(1), working: Some(Box::new(working.clone())) };
        catalog.apply(working_op.clone()).unwrap();
        let binding = Op::SetAsset { id: id.clone(), asset: Some(Box::new(rebound.clone())) };
        let inverse = catalog.apply(binding.clone()).unwrap();
        let rebound_working = catalog.working_copies().get(&crate::PhotoId(1)).unwrap().clone();
        assert_eq!(rebound_working.connection, "second-device");
        catalog.apply(Op::SetWorkingCopy { id: crate::PhotoId(1), working: Some(Box::new(rebound_working)) }).unwrap();
        catalog.apply(inverse).unwrap();
        assert_eq!(catalog.asset(&id), Some(&asset));
        assert_eq!(catalog.working_copies().get(&crate::PhotoId(1)), Some(&working));
        catalog.apply(binding.clone()).unwrap();
        // Same manifest/binding is idempotent, and rebinding survives journal replay and compaction.
        catalog.apply(binding.clone()).unwrap();
        let store = MemStore::default();
        let (mut journal, _, _) = Journal::open(Box::new(store.clone())).unwrap();
        journal.append(&[first, working_op, binding]).unwrap();
        let (_, replayed, _) = Journal::open(Box::new(store.clone())).unwrap();
        assert_eq!(replayed.asset(&id), Some(&rebound));
        assert_eq!(replayed.working_copies(), catalog.working_copies());
        journal.snapshot(&catalog).unwrap();
        let (_, compacted, _) = Journal::open(Box::new(store)).unwrap();
        assert_eq!(compacted.asset(&id), Some(&rebound));
        assert_eq!(compacted.working_copies(), catalog.working_copies());
    }

    #[test]
    fn assets_are_shared_by_virtual_copies_and_roundtrip_in_the_journal() {
        use crate::{Catalog, Journal, MemStore, Op, Photo, PhotoId, Source};
        let mut catalog = Catalog::new();
        let id = AssetId("shared-original".into());
        let asset = Asset { associated: Default::default(), original: blob(), smart_preview: None, thumbnail: None };
        let mut copy = Photo::new(PhotoId(2), Source::Managed { asset_id: id.clone() }, "photo.raw", "RAW", 12, 8, "2026-10-12");
        copy.copy_of = Some(PhotoId(1));
        let ops = vec![
            Op::SetAsset { id: id.clone(), asset: Some(Box::new(asset.clone())) },
            Op::AddPhoto { photo: Box::new(Photo::new(PhotoId(1), copy.source.clone(), "photo.raw", "RAW", 12, 8, "2026-10-12")) },
            Op::AddPhoto { photo: Box::new(copy) },
        ];
        let store = MemStore::default();
        let (mut journal, _, _) = Journal::open(Box::new(store.clone())).unwrap();
        for op in &ops {
            catalog.apply(op.clone()).unwrap();
        }
        journal.append(&ops).unwrap();
        let (_, loaded, _) = Journal::open(Box::new(store.clone())).unwrap();
        assert_eq!(loaded.assets(), catalog.assets());
        assert_eq!(loaded.photos().collect::<Vec<_>>(), catalog.photos().collect::<Vec<_>>());
        journal.snapshot(&catalog).unwrap();
        let (_, compacted, _) = Journal::open(Box::new(store)).unwrap();
        assert_eq!(serde_json::to_value(&compacted).unwrap(), serde_json::to_value(&catalog).unwrap());
        assert_eq!(loaded.assets().len(), 1);
        assert_eq!(loaded.photo(PhotoId(1)).unwrap().source, loaded.photo(PhotoId(2)).unwrap().source);
        assert!(catalog.apply(Op::SetAsset { id: id.clone(), asset: None }).is_err());
        let mut changed = asset;
        changed.original.id = BlobId("b".repeat(64));
        assert!(catalog.apply(Op::SetAsset { id, asset: Some(Box::new(changed)) }).is_err());
    }

    #[test]
    fn legacy_sources_and_catalogs_load_without_cloud_fields() {
        use crate::{Catalog, Op, Photo, PhotoId, Source};
        let source: Source = serde_json::from_str(r#"{"type":"file","path":"old.raw"}"#).unwrap();
        let mut catalog = Catalog::new();
        catalog.apply(Op::AddPhoto { photo: Box::new(Photo::new(PhotoId(1), source.clone(), "old.raw", "RAW", 12, 8, "2026-10-12")) }).unwrap();
        let value = serde_json::to_value(&catalog).unwrap();
        assert!(value.get("assets").is_none());
        let loaded: Catalog = serde_json::from_value(value).unwrap();
        assert!(loaded.assets().is_empty());
        assert_eq!(loaded.photo(PhotoId(1)).unwrap().source, source);
        let store = crate::MemStore::default();
        store.set(
            crate::journal::SNAPSHOT,
            serde_json::to_vec(&serde_json::json!({
                "format": "lightcraft-catalog", "version": 7, "seq": 0, "catalog": loaded,
            }))
            .unwrap(),
        );
        let (_, upgraded, report) = crate::Journal::open(Box::new(store.clone())).unwrap();
        assert_eq!(report.upgraded_from, Some(7));
        assert_eq!(upgraded.photo(PhotoId(1)).unwrap().source, source);
        let snap: serde_json::Value = serde_json::from_slice(&store.get(crate::journal::SNAPSHOT).unwrap()).unwrap();
        assert_eq!(snap["version"], crate::journal::VERSION);
    }

    #[test]
    fn exhausted_photo_identity_is_an_error_instead_of_an_overflow() {
        let mut catalog = crate::Catalog::new();
        let photo = crate::Photo::new(crate::PhotoId(u64::MAX), crate::Source::Demo { scene: 0 }, "photo", "DEMO", 1, 1, "today");
        assert!(catalog.apply(crate::Op::AddPhoto { photo: Box::new(photo) }).is_err());
    }
}
