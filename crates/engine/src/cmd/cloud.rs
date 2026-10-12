//! Managed cloud asset commands shared by UI, CLI, control channel and MCP.

use super::{CommandSpec, always, bad, cmd};
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
    ]
}
