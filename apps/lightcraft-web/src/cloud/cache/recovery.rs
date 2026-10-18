//! Bounded reconstruction of cache metadata from verified bytes and durable selections.
use super::{Entry, Owner, Policy};
use crate::files::Files;
use lightcraft_engine::{assets::cache::OfflinePin, catalog::Journal};
use std::collections::BTreeMap;

pub const MAX_FILES: usize = 100_000;
#[cfg(target_arch = "wasm32")]
pub const MAX_LIBRARIES: usize = 4096;
pub const MAX_CATALOG_FILE: u64 = 128 << 20;
#[cfg(target_arch = "wasm32")]
pub const MAX_METADATA: u64 = 512 << 20;
#[cfg(target_arch = "wasm32")]
pub const MAX_AUDIT_BYTES: u64 = 16 << 30;

/// The exact durable catalog and owner intent, not a previous resolved blob snapshot.
/// Empty owner selections are retained so later membership changes still take effect.
pub fn owners(
    dir: &str,
    snapshot: Option<Vec<u8>>,
    journal: Option<Vec<u8>>,
    intent: Option<Vec<u8>>,
    legacy: Option<&BTreeMap<String, Owner>>,
) -> Result<Option<BTreeMap<String, Owner>>, String> {
    let selected = match intent {
        Some(bytes) => super::decode_intent(&bytes)?,
        None => legacy
            .into_iter()
            .flat_map(|owners| owners.iter())
            .map(|(owner, pin)| (owner.clone(), OfflinePin { quality: pin.quality, blobs: Vec::new() }))
            .collect(),
    };
    if selected.is_empty() {
        return Ok(Some(BTreeMap::new()));
    }
    let (Some(snapshot), Some(journal)) = (snapshot, journal) else {
        return Err(format!("Offline selections for {dir} cannot be checked because its catalog is missing; stored copies are retained."));
    };
    if snapshot.len() as u64 > MAX_CATALOG_FILE || journal.len() as u64 > MAX_CATALOG_FILE {
        return Err("Browser offline catalog exceeds the recovery limit; copies retained.".into());
    }
    let files = Files::default();
    files.preload("catalog.snap", snapshot);
    files.preload("catalog.log", journal);
    let (_, catalog, report) =
        Journal::open(Box::new(files.store())).map_err(|e| format!("Cannot check offline catalog {dir}: {e}; stored copies retained."))?;
    if report.created || report.damaged.is_some() || report.failed > 0 || report.torn_bytes > 0 || report.torn_fragments > 0 {
        return Err(format!("Offline catalog {dir} needs repair; stored copies retained."));
    }
    let resolved = lightcraft_engine::assets::offline::resolve(&catalog, &selected);
    if !resolved.errors.is_empty() {
        return Err(format!("Offline selections for {dir} need repair; stored copies retained."));
    }
    let owners = resolved
        .pins
        .into_iter()
        .map(|(id, pin)| (id, Owner { quality: pin.quality, blobs: pin.blobs.into_iter().map(|id| id.0).collect() }))
        .collect();
    Ok(Some(owners))
}

/// The caller hashes the bounded actual file first. Unknown or corrupt files are
/// retained and stop reclamation; a filename alone never certifies disposable data.
pub fn verified_entry(id: &str, size: u64, digest: &str) -> Result<(String, u64), String> {
    if !super::hash(id) || size == 0 || size > super::super::MAX_BROWSER_BLOB || id != digest {
        return Err("Browser cache contains an unverified file; stored copies retained for repair.".into());
    }
    Ok((id.into(), size))
}
pub fn entries(policy: &mut Policy, verified: BTreeMap<String, u64>) -> Result<(), String> {
    if verified.len() > MAX_FILES {
        return Err("Too many browser cache files to recover safely; copies retained.".into());
    }
    policy.entries = verified
        .into_iter()
        .map(|(id, size)| {
            let used = policy.entries.get(&id).map_or(0, |entry| entry.used);
            (id, Entry { size, used })
        })
        .collect();
    // Reuse wire validation, including aggregate owner and file bounds.
    Policy::decode(&serde_json::to_vec(policy).map_err(|e| e.to_string())?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_engine::{
        Session,
        assets::cache::{OfflinePin, OfflineQuality},
        catalog::{
            MemStore, Op, Photo, PhotoId, Source,
            assets::{Asset, AssetId, Blob, BlobId},
        },
    };
    fn fixture(bytes: &[u8]) -> (Session, Files) {
        let files = Files::default();
        let mut s = Session::new();
        s.open_library_in(
            lightcraft_engine::library::LibraryStores {
                dir: "recovery-fixture".into(),
                catalog: Box::new(files.store()),
                files: Box::new(MemStore::new()),
                on_disk: false,
            },
            false,
        )
        .unwrap();
        replace(&mut s, bytes);
        (s, files)
    }
    fn replace(s: &mut Session, bytes: &[u8]) {
        let asset_id = AssetId(lightcraft_engine::assets::sha256(bytes).0);
        let blob = Blob {
            id: BlobId(lightcraft_engine::assets::sha256(bytes).0),
            size: bytes.len() as u64,
            remote: lightcraft_engine::catalog::assets::RemoteLocation { connection: "memory".into(), key: "library/original".into(), version: None },
        };
        s.commit(
            "fixture",
            Op::SetAsset {
                id: asset_id.clone(),
                asset: Some(Box::new(Asset { original: blob, smart_preview: None, thumbnail: None, associated: Default::default() })),
            },
        )
        .unwrap();
        if s.catalog.photo(PhotoId(1)).is_none() {
            s.commit("fixture", Op::AddPhoto { photo: Box::new(Photo::new(PhotoId(1), Source::Managed { asset_id }, "a.raw", "RAW", 32, 24, "")) })
                .unwrap();
        } else {
            s.commit("replace fixture source", Op::SetFile { id: PhotoId(1), file_name: "a.raw".into(), source: Source::Managed { asset_id } })
                .unwrap();
        }
        s.persist().unwrap();
    }
    fn selected() -> Vec<u8> {
        super::super::encode_intent(
            &[
                ("photo:1".into(), OfflinePin { quality: OfflineQuality::Original, blobs: Vec::new() }),
                ("album:999".into(), OfflinePin { quality: OfflineQuality::Original, blobs: Vec::new() }),
            ]
            .into(),
        )
        .unwrap()
    }
    fn resolve(files: &Files, legacy: Option<&BTreeMap<String, Owner>>) -> BTreeMap<String, Owner> {
        owners("library", files.get("catalog.snap"), files.get("catalog.log"), Some(selected()), legacy).unwrap().unwrap()
    }
    #[test]
    fn closed_catalogs_recompute_live_intent_and_preserve_shared_and_empty_owners() {
        let (mut a, af) = fixture(b"shared");
        let (_, bf) = fixture(b"shared");
        let shared = lightcraft_engine::assets::sha256(b"shared").0;
        let old = resolve(&af, None);
        replace(&mut a, b"replacement");
        let replacement = lightcraft_engine::assets::sha256(b"replacement").0;
        let mut p = Policy::default();
        p.libraries.insert("library".into(), resolve(&af, Some(&old)));
        p.libraries.insert("library-restored-1".into(), resolve(&bf, None));
        assert_eq!(p.libraries["library"]["photo:1"].blobs, [replacement.clone()].into());
        assert!(p.libraries["library"]["album:999"].blobs.is_empty());
        assert_eq!(p.protected(), [shared.as_str(), replacement.as_str()].into());
    }
    #[test]
    fn damaged_missing_and_unknown_intents_never_authorize_reclamation() {
        let (_, files) = fixture(b"a");
        assert!(owners("library", files.get("catalog.snap"), None, Some(selected()), None).is_err());
        let mut log = files.get("catalog.log").unwrap();
        log.extend_from_slice(b"{\"partial\":");
        assert!(owners("library", files.get("catalog.snap"), Some(log), Some(selected()), None).is_err());
        assert!(
            owners("library", files.get("catalog.snap"), files.get("catalog.log"), Some(br#"{"version":99,"owners":{}}"#.to_vec()), None).is_err()
        );
    }
    #[test]
    fn actual_verified_files_replace_stale_sizes_and_adopt_orphans_without_guessing() {
        let id = lightcraft_engine::assets::sha256(b"cached").0;
        let gone = "a".repeat(64);
        let mut p = Policy::default();
        p.entries.insert(gone, Entry { size: 100, used: 1 });
        entries(&mut p, [verified_entry(&id, 6, &id).unwrap()].into()).unwrap();
        assert_eq!(p.bytes(), 6);
        assert_eq!(p.entries.len(), 1);
        assert!(verified_entry(&id, 6, &"b".repeat(64)).is_err());
        assert!(verified_entry("unknown", 6, &id).is_err());
        assert!(verified_entry(&id, 0, &id).is_err());
    }
}
