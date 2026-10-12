//! Managed cloud asset commands shared by UI, CLI, control channel and MCP.

use super::{CommandSpec, always, bad, cmd};
use crate::assets::cache::{OfflinePin, OfflineQuality};
use crate::assets::{AssetResolver, SourceRequirement, TransferManager};
use crate::{Result, Selection, Session};
use lightcraft_catalog::assets::{Asset, AssetId};
use lightcraft_catalog::{MediaKind, Op, Photo, Source};
use serde_json::{Value, json};
use std::sync::Arc;

fn resolver(s: &Session, command: &str) -> Result<Arc<AssetResolver>> {
    s.media.asset_resolver.clone().ok_or_else(|| bad(command, "open a native persistent library first"))
}

fn configure(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "cloud.configure";
    let connection: crate::assets::s3::S3Connection =
        serde_json::from_value(p.clone()).map_err(|_| bad(C, "invalid connection settings; credentials must not be supplied in parameters"))?;
    connection.validate().map_err(|e| bad(C, e.to_string()))?;
    let resolver = resolver(s, C)?;
    let lib = s.library.as_mut().ok_or_else(|| bad(C, "open a library first"))?;
    let mut connections = lib.cloud_connections()?;
    connections.retain(|c| c.id != connection.id);
    if connections.len() >= 64 {
        return Err(bad(C, "at most 64 cloud connections are supported"));
    }
    connections.push(connection.clone());
    lib.save_cloud_connections(&connections)?;
    resolver.connect(connection.id.clone(), Arc::new(connection)).map_err(|e| bad(C, e.to_string()))?;
    resume_offline_pins(s);
    Ok(json!({"connections": connections}))
}

fn attach(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "cloud.attach";
    let _ = resolver(s, C)?;
    let id = AssetId(super::str_param(p, "assetId").ok_or_else(|| bad(C, "missing assetId"))?.into());
    if !lightcraft_catalog::assets::identifier(&id.0) {
        return Err(bad(C, "invalid assetId"));
    }
    let asset: Asset =
        serde_json::from_value(p.get("asset").cloned().ok_or_else(|| bad(C, "missing asset"))?).map_err(|_| bad(C, "invalid asset record"))?;
    asset.validate().map_err(|e| bad(C, e))?;
    if s.catalog.asset(&id).is_some_and(|existing| existing != &asset) {
        return Err(bad(C, "asset identity already describes different bytes or renditions"));
    }
    let name = super::str_param(p, "fileName").ok_or_else(|| bad(C, "missing fileName"))?;
    if name.is_empty() || name.len() > 1024 || name.contains(['/', '\\']) || name.chars().any(char::is_control) {
        return Err(bad(C, "invalid fileName"));
    }
    let dimension = |key| {
        p.get(key)
            .and_then(Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .filter(|v| *v > 0 && *v <= 100_000)
            .ok_or_else(|| bad(C, format!("invalid {key}")))
    };
    let (width, height) = (dimension("width")?, dimension("height")?);
    if width > 65_535 || height > 65_535 || u64::from(width).saturating_mul(u64::from(height)) > 100_000_000 {
        return Err(bad(C, "managed photo dimensions exceed the 100 MP / 65,535-pixel safety limit"));
    }
    let kind: MediaKind = serde_json::from_value(p.get("kind").cloned().unwrap_or(json!("image"))).map_err(|_| bad(C, "invalid media kind"))?;
    if kind == MediaKind::Video {
        return Err(bad(C, "managed video is not supported"));
    }
    let format = super::str_param(p, "format").unwrap_or("RAW");
    if format.len() > 32 || format.chars().any(char::is_control) {
        return Err(bad(C, "invalid format"));
    }
    let photo_id = s.catalog.alloc_photo_id();
    let mut photo = Photo::new(photo_id, Source::Managed { asset_id: id.clone() }, name, format, width, height, &(s.clock)());
    photo.kind = kind;
    photo.file_size = asset.original.size;
    let mut ops = Vec::new();
    if s.catalog.asset(&id).is_none() {
        ops.push(Op::SetAsset { id: id.clone(), asset: Some(Box::new(asset)) });
    }
    ops.push(Op::AddPhoto { photo: Box::new(photo) });
    s.commit("Add Cloud Photo", Op::Batch { ops })?;
    s.selection = Selection::single(photo_id);
    Ok(json!({"id": photo_id.0, "assetId": id.0, "savedLocally": true}))
}

fn status(s: &mut Session, p: &Value) -> Result<Value> {
    let resolver = resolver(s, "cloud.status")?;
    let photos = s
        .targets(p)
        .into_iter()
        .filter_map(|id| {
            let photo = s.catalog.photo(id)?;
            let Source::Managed { asset_id } = &photo.source else { return None };
            let asset = s.catalog.asset(asset_id)?;
            Some(json!({"id": id.0, "assetId": asset_id.0, "original": resolver.availability(&asset.original.id),
            "smartPreview": asset.smart_preview.as_ref().map(|b| resolver.availability(&b.id))}))
        })
        .collect::<Vec<_>>();
    Ok(json!({"photos": photos}))
}

fn fetch(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "cloud.fetch";
    let resolver = resolver(s, C)?;
    let ids = s.targets(p);
    if ids.is_empty() || ids.len() > 1000 {
        return Err(bad(C, "select between 1 and 1000 photos"));
    }
    let mut assets = Vec::new();
    for id in ids {
        let photo = s.catalog.photo(id).ok_or_else(|| bad(C, format!("no photo {}", id.0)))?;
        let Source::Managed { asset_id } = &photo.source else {
            return Err(bad(C, "select managed cloud photos"));
        };
        assets.push(s.catalog.asset(asset_id).cloned().ok_or_else(|| bad(C, "missing asset record"))?);
    }
    let requirement = if super::bool_or(p, "original", true) { SourceRequirement::OriginalRequired } else { SourceRequirement::PreviewAllowed };
    let task = s.activity.start("download", "Download Cloud Photos", crate::activity::Cancel::Yes);
    let cancel = task.cancel_flag().ok_or_else(|| bad(C, "download cancellation flag could not be created"))?;
    let task_id = task.id();
    let total = assets.len();
    std::thread::Builder::new()
        .name("cloud-download".into())
        .spawn(move || {
            for (i, asset) in assets.iter().enumerate() {
                if let Err(e) = resolver.acquire_asset(asset, requirement, &cancel) {
                    log::warn!("cloud download: {e}");
                    break;
                }
                task.progress(i as u64 + 1, total as u64);
            }
        })
        .map_err(|e| bad(C, format!("could not start download worker: {e}")))?;
    Ok(json!({"background": true, "task": task_id, "total": total}))
}

fn cache(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "cloud.cache";
    let resolver = resolver(s, C)?;
    if let Some(value) = p.get("budgetMb") {
        let mb = value.as_u64().filter(|v| *v <= (1 << 30)).ok_or_else(|| bad(C, "budgetMb must be between 0 and 1,073,741,824"))?;
        resolver.set_cache_budget(mb << 20).map_err(|e| bad(C, e.to_string()))?;
    }
    Ok(json!(resolver.cache_status()))
}

fn pin_targets(s: &Session, p: &Value, command: &str) -> Result<(Option<String>, Vec<lightcraft_catalog::PhotoId>)> {
    if let Some(value) = p.get("albumId") {
        let id = lightcraft_catalog::AlbumId(value.as_u64().filter(|id| *id > 0).ok_or_else(|| bad(command, "invalid albumId"))?);
        if s.catalog.album(id).is_none() {
            return Err(bad(command, "album does not exist"));
        }
        return Ok((Some(format!("album:{}", id.0)), s.catalog.album_photos(id)));
    }
    Ok((None, s.targets(p)))
}

fn pin(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "cloud.pin";
    let resolver = resolver(s, C)?;
    let quality: OfflineQuality =
        serde_json::from_value(p.get("quality").cloned().unwrap_or(json!("original"))).map_err(|_| bad(C, "quality must be original or preview"))?;
    let (album, ids) = pin_targets(s, p, C)?;
    if ids.is_empty() || ids.len() > 100_000 {
        return Err(bad(C, "select between 1 and 100,000 photos"));
    }
    let mut changes = Vec::new();
    let mut album_blobs = std::collections::BTreeSet::new();
    for id in ids {
        let photo = s.catalog.photo(id).ok_or_else(|| bad(C, "photo does not exist"))?;
        let Source::Managed { asset_id } = &photo.source else {
            if album.is_some() {
                continue;
            }
            return Err(bad(C, "select managed cloud photos"));
        };
        let asset = s.catalog.asset(asset_id).ok_or_else(|| bad(C, "missing asset record"))?;
        let blob = match quality {
            OfflineQuality::Original => &asset.original,
            OfflineQuality::Preview => asset
                .smart_preview
                .as_ref()
                .ok_or_else(|| bad(C, format!("{} has no editable smart preview; pin its original instead", photo.file_name)))?,
        };
        if album.is_some() {
            album_blobs.insert(blob.id.clone());
        } else {
            changes.push((format!("photo:{}", id.0), Some(OfflinePin { quality, blobs: vec![blob.id.clone()] })));
        }
    }
    if let Some(owner) = album {
        if album_blobs.is_empty() {
            return Err(bad(C, "album has no managed cloud photos"));
        }
        changes.push((owner, Some(OfflinePin { quality, blobs: album_blobs.into_iter().collect() })));
    }
    resolver.set_offline_pins(&changes).map_err(|e| bad(C, e.to_string()))?;
    resume_offline_pins(s);
    Ok(json!({"savedLocally": true, "pins": changes.len(), "quality": quality}))
}

fn unpin(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "cloud.unpin";
    let resolver = resolver(s, C)?;
    let owners = if let Some(owner) = super::str_param(p, "owner") {
        vec![owner.to_owned()]
    } else {
        let (album, ids) = pin_targets(s, p, C)?;
        match album {
            Some(owner) => vec![owner],
            None => ids.iter().map(|id| format!("photo:{}", id.0)).collect(),
        }
    };
    if owners.is_empty() || owners.len() > 100_000 {
        return Err(bad(C, "select photos or specify an offline pin owner"));
    }
    let changes: Vec<_> = owners.into_iter().map(|owner| (owner, None)).collect();
    resolver.set_offline_pins(&changes).map_err(|e| bad(C, e.to_string()))?;
    Ok(json!({"unpinned": changes.len()}))
}

fn remove_local_copy(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "cloud.removeLocalCopy";
    let resolver = resolver(s, C)?;
    let (album, ids) = pin_targets(s, p, C)?;
    if ids.is_empty() || ids.len() > 100_000 {
        return Err(bad(C, "select between 1 and 100,000 photos"));
    }
    let mut blobs = std::collections::BTreeMap::new();
    for id in ids {
        let photo = s.catalog.photo(id).ok_or_else(|| bad(C, "photo does not exist"))?;
        let Source::Managed { asset_id } = &photo.source else {
            if album.is_some() {
                continue;
            }
            return Err(bad(C, "select managed cloud photos"));
        };
        let asset = s.catalog.asset(asset_id).ok_or_else(|| bad(C, "missing asset record"))?;
        // Keep editable previews and rendered thumbnails; remove the disposable original only.
        blobs.insert(asset.original.id.clone(), asset.original.clone());
    }
    // Virtual copies share bytes. Release all their decoded sources, leaving live worker
    // leases protected; an in-flight operation produces an actionable retry error.
    let affected: Vec<_> = s
        .catalog
        .photos()
        .filter(|photo| {
            let Source::Managed { asset_id } = &photo.source else { return false };
            s.catalog.asset(asset_id).is_some_and(|asset| blobs.contains_key(&asset.original.id))
        })
        .map(|photo| photo.id)
        .collect();
    for id in affected {
        s.media.forget(id);
    }
    let mut removed = Vec::new();
    let mut failed = Vec::new();
    for (id, blob) in blobs {
        match resolver.evict(&blob) {
            Ok(()) => removed.push(id),
            Err(error) => failed.push(json!({"blob": id, "error": error.to_string()})),
        }
    }
    Ok(json!({"removed": removed, "failed": failed}))
}

/// Reconnecting/reopening retries durable offline intent. It never mutates the catalog.
pub(crate) fn resume_offline_pins(s: &Session) {
    let Some(resolver) = s.media.asset_resolver.clone() else { return };
    let status = resolver.cache_status();
    if let Some(error) = status.error {
        log::warn!("offline pin recovery: {error}");
        return;
    }
    let wanted: std::collections::BTreeSet<_> = status.pins.values().flat_map(|pin| pin.blobs.iter().cloned()).collect();
    if wanted.is_empty() {
        return;
    }
    let blobs: std::collections::BTreeMap<_, _> = s
        .catalog
        .assets()
        .values()
        .flat_map(|asset| std::iter::once(&asset.original).chain(asset.smart_preview.iter()).chain(asset.thumbnail.iter()))
        .filter(|blob| wanted.contains(&blob.id))
        .map(|blob| (blob.id.clone(), blob.clone()))
        .collect();
    let task = s.activity.start("download", "Download Offline Copies", crate::activity::Cancel::Yes);
    let Some(cancel) = task.cancel_flag() else { return };
    let total = blobs.len() as u64;
    if let Err(error) = std::thread::Builder::new().name("cloud-offline".into()).spawn(move || {
        for (i, blob) in blobs.values().enumerate() {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            if let Err(error) = resolver.acquire(blob, &cancel) {
                log::warn!("offline copy: {error}");
            }
            task.progress(i as u64 + 1, total);
        }
    }) {
        log::warn!("offline copy worker: {error}");
    }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "cloud.configure",
            "Connect Cloud Storage",
            [],
            None,
            "{id, bucket, region, prefix?, endpoint?, profile?}; credentials resolved from AWS environment or shared profile",
            always,
            configure
        ),
        cmd!(
            "cloud.attach",
            "Add Cloud Photo",
            [],
            None,
            "{assetId, asset: {original, smartPreview?, thumbnail?}, fileName, width, height, kind?, format?}; attach already uploaded objects without downloading",
            always,
            attach
        ),
        cmd!(query "cloud.status", "Cloud Photo Status", [], None, "{ids?}; cached, downloading, remote, missing or failed state on this device", always, status),
        cmd!(query "cloud.fetch", "Download Cloud Photos", [], None, "{ids?, original?: true}; background download, cancellation via activity.cancel", always, fetch),
        cmd!(query "cloud.cache", "Cloud Cache Settings", [], None, "{budgetMb?}; device-local disk budget, usage and offline pins; 0 retains only pinned or active bytes", always, cache),
        cmd!(query "cloud.pin", "Keep Cloud Photos Offline", [], None, "{ids? or albumId?, quality?: original|preview}; persist pin intent and download in background; album pins snapshot current members", always, pin),
        cmd!(query "cloud.unpin", "Release Offline Pins", [], None, "{ids? or albumId? or owner?}; release only these owners' pins", always, unpin),
        cmd!(query "cloud.removeLocalCopy", "Remove Local Original Copy", [], None, "{ids? or albumId?}; preserve library/remote objects/previews; return removed and failed blobs; refuse pinned or in-use bytes", always, remove_local_copy),
    ]
}
